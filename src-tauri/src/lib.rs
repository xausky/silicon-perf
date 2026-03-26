use futures_util::stream::{self, StreamExt};
use reqwest::header::USER_AGENT;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EndpointConfig {
    name: Option<String>,
    base_url: String,
    api_key: String,
    models: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkRequest {
    endpoints: Vec<EndpointConfig>,
    prompt: String,
    rounds: u32,
    concurrency: Option<usize>,
    max_tokens: Option<u32>,
    temperature: Option<f64>,
    user_agent: Option<String>,
}

#[derive(Debug, Clone)]
struct BenchmarkTask {
    index: usize,
    endpoint_name: String,
    base_url: String,
    api_key: String,
    model: String,
    round: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkResult {
    index: usize,
    endpoint_name: String,
    base_url: String,
    model: String,
    round: u32,
    success: bool,
    status: String,
    first_token_latency_secs: Option<f64>,
    output_speed_tps: Option<f64>,
    result: String,
}

fn normalize_base_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

fn estimate_tokens(text: &str) -> f64 {
    let chars = text.chars().count().max(1) as f64;
    (chars / 4.0).max(1.0)
}

fn default_user_agent() -> String {
    format!("silicon-perf/{}", env!("CARGO_PKG_VERSION"))
}

fn resolve_user_agent(request: &BenchmarkRequest) -> String {
    match request.user_agent.as_deref().map(str::trim) {
        Some(ua) if !ua.is_empty() => ua.to_string(),
        _ => default_user_agent(),
    }
}

fn process_sse_event(
    event: &str,
    output: &mut String,
    first_token_latency_secs: &mut Option<f64>,
    completion_tokens: &mut Option<f64>,
    started_at: Instant,
) -> Result<bool, String> {
    let mut payload_parts: Vec<&str> = Vec::new();

    for line in event.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(':') {
            continue;
        }

        if let Some(data) = trimmed.strip_prefix("data:") {
            payload_parts.push(data.trim());
        }
    }

    if payload_parts.is_empty() {
        return Ok(false);
    }

    let payload = payload_parts.join("\n");
    if payload == "[DONE]" {
        return Ok(true);
    }

    let parsed: Value = serde_json::from_str(&payload)
        .map_err(|e| format!("解析 SSE 数据失败: {e}; payload={payload}"))?;

    if let Some(content) = parsed
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        output.push_str(content);
        if first_token_latency_secs.is_none() {
            *first_token_latency_secs = Some(started_at.elapsed().as_secs_f64());
        }
    }

    if let Some(tokens) = parsed
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_f64)
    {
        *completion_tokens = Some(tokens.max(1.0));
    }

    Ok(false)
}

async fn run_single_task(
    client: &Client,
    task: BenchmarkTask,
    request: &BenchmarkRequest,
    user_agent: &str,
) -> BenchmarkResult {
    let started_at = Instant::now();
    let endpoint = format!("{}/chat/completions", normalize_base_url(&task.base_url));

    let body = json!({
        "model": task.model,
        "messages": [
            {
                "role": "user",
                "content": request.prompt
            }
        ],
        "stream": true,
        "stream_options": {
            "include_usage": true
        },
        "temperature": request.temperature.unwrap_or(0.0),
        "max_tokens": request.max_tokens.unwrap_or(2048)
    });

    let response = match client
        .post(endpoint)
        .header(USER_AGENT, user_agent)
        .bearer_auth(task.api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            return BenchmarkResult {
                index: task.index,
                endpoint_name: task.endpoint_name,
                base_url: task.base_url,
                model: task.model,
                round: task.round,
                success: false,
                status: "失败".to_string(),
                first_token_latency_secs: None,
                output_speed_tps: None,
                result: format!("请求失败: {e}"),
            }
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "读取错误响应失败".to_string());
        return BenchmarkResult {
            index: task.index,
            endpoint_name: task.endpoint_name,
            base_url: task.base_url,
            model: task.model,
            round: task.round,
            success: false,
            status: "失败".to_string(),
            first_token_latency_secs: None,
            output_speed_tps: None,
            result: format!("HTTP {}: {}", status.as_u16(), text),
        };
    }

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut output = String::new();
    let mut first_token_latency_secs = None;
    let mut completion_tokens = None;
    let mut done = false;

    while let Some(next_chunk) = stream.next().await {
        let chunk = match next_chunk {
            Ok(bytes) => bytes,
            Err(e) => {
                return BenchmarkResult {
                    index: task.index,
                    endpoint_name: task.endpoint_name,
                    base_url: task.base_url,
                    model: task.model,
                    round: task.round,
                    success: false,
                    status: "失败".to_string(),
                    first_token_latency_secs,
                    output_speed_tps: None,
                    result: format!("读取流失败: {e}"),
                }
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(idx) = buffer.find("\n\n") {
            let event = buffer[..idx].to_string();
            buffer.drain(..idx + 2);

            match process_sse_event(
                &event,
                &mut output,
                &mut first_token_latency_secs,
                &mut completion_tokens,
                started_at,
            ) {
                Ok(is_done) => {
                    if is_done {
                        done = true;
                        break;
                    }
                }
                Err(e) => {
                    return BenchmarkResult {
                        index: task.index,
                        endpoint_name: task.endpoint_name,
                        base_url: task.base_url,
                        model: task.model,
                        round: task.round,
                        success: false,
                        status: "失败".to_string(),
                        first_token_latency_secs,
                        output_speed_tps: None,
                        result: e,
                    }
                }
            }
        }

        if done {
            break;
        }
    }

    let total_secs = started_at.elapsed().as_secs_f64().max(0.001);
    let tokens = completion_tokens.unwrap_or_else(|| estimate_tokens(&output));
    let output_speed_tps = (tokens / total_secs).max(0.0);
    let safe_result = if output.trim().is_empty() {
        "(空响应)".to_string()
    } else {
        output
    };

    BenchmarkResult {
        index: task.index,
        endpoint_name: task.endpoint_name,
        base_url: task.base_url,
        model: task.model,
        round: task.round,
        success: true,
        status: "成功".to_string(),
        first_token_latency_secs: Some(first_token_latency_secs.unwrap_or(total_secs)),
        output_speed_tps: Some(output_speed_tps),
        result: safe_result,
    }
}

#[tauri::command]
async fn run_benchmark(request: BenchmarkRequest) -> Result<Vec<BenchmarkResult>, String> {
    if request.endpoints.is_empty() {
        return Err("请至少配置一个 endpoint".to_string());
    }
    if request.prompt.trim().is_empty() {
        return Err("提示词不能为空".to_string());
    }

    let rounds = request.rounds.max(1);
    let mut tasks = Vec::new();

    for endpoint in &request.endpoints {
        let endpoint_name = endpoint
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| endpoint.base_url.clone());

        for model in endpoint.models.iter().filter(|m| !m.trim().is_empty()) {
            for round in 1..=rounds {
                tasks.push(BenchmarkTask {
                    index: tasks.len(),
                    endpoint_name: endpoint_name.clone(),
                    base_url: endpoint.base_url.clone(),
                    api_key: endpoint.api_key.clone(),
                    model: model.trim().to_string(),
                    round,
                });
            }
        }
    }

    if tasks.is_empty() {
        return Err("没有可测试的 model 组合".to_string());
    }

    let concurrency = request.concurrency.unwrap_or(8).max(1).min(256);
    let user_agent = Arc::new(resolve_user_agent(&request));
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let client = Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("初始化 HTTP 客户端失败: {e}"))?;

    let request = Arc::new(request);

    let mut results: Vec<BenchmarkResult> = stream::iter(tasks)
        .map(|task| {
            let semaphore = Arc::clone(&semaphore);
            let client = client.clone();
            let request = Arc::clone(&request);
            let user_agent = Arc::clone(&user_agent);
            async move {
                let _permit = semaphore.acquire_owned().await.map_err(|e| e.to_string())?;
                Ok::<BenchmarkResult, String>(
                    run_single_task(&client, task, &request, user_agent.as_str()).await,
                )
            }
        })
        .buffer_unordered(concurrency)
        .filter_map(|item| async move { item.ok() })
        .collect()
        .await;

    results.sort_by_key(|r| r.index);
    Ok(results)
}

#[tauri::command]
fn get_default_user_agent() -> String {
    default_user_agent()
}

#[tauri::command]
fn save_config_file(path: String, content: String) -> Result<(), String> {
    fs::write(&path, content).map_err(|e| format!("写入文件失败: {e}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            run_benchmark,
            get_default_user_agent,
            save_config_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
