<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { save } from "@tauri-apps/plugin-dialog";
  import { onMount } from "svelte";

  type EndpointForm = {
    name: string;
    baseUrl: string;
    apiKey: string;
    modelsInput: string;
  };

  type EndpointRequest = {
    name: string;
    baseUrl: string;
    apiKey: string;
    models: string[];
  };

  type ResultRow = {
    endpointName: string;
    baseUrl: string;
    model: string;
    rounds: number;
    status: "测试中" | "成功" | "失败";
    firstTokenLatencySecs: number | null;
    firstTokenMinusSecs: number | null;
    firstTokenPlusSecs: number | null;
    outputSpeedTps: number | null;
    outputMinusTps: number | null;
    outputPlusTps: number | null;
    success: boolean | null;
    successCount: number;
    failCount: number;
    failedRounds: number[];
    roundResults: BenchmarkResult[];
    result: string;
  };

  type BenchmarkResult = {
    index: number;
    endpointName: string;
    baseUrl: string;
    model: string;
    round: number;
    success: boolean;
    status: "成功" | "失败";
    firstTokenLatencySecs: number | null;
    outputSpeedTps: number | null;
    result: string;
  };

  type BenchmarkProgress = {
    item: BenchmarkResult;
    completed: number;
    total: number;
  };

  type RunSettings = {
    normalizedEndpoints: EndpointRequest[];
    safeRounds: number;
    safeConcurrency: number;
    safeMaxTokens: number;
  };

  type SortDirection = "asc" | "desc";
  type SortKey = keyof Pick<
    ResultRow,
    "endpointName" | "model" | "firstTokenLatencySecs" | "outputSpeedTps" | "result" | "status"
  >;

  type SavedConfig = {
    version: number;
    endpoints: EndpointForm[];
    prompt: string;
    rounds: number;
    concurrency: number;
    maxTokens: number;
    temperature: number;
    userAgent: string;
  };

  const createEndpoint = (): EndpointForm => ({
    name: "",
    baseUrl: "",
    apiKey: "",
    modelsInput: "gpt-4o-mini"
  });

  const createRow = (endpoint: EndpointRequest, model: string, rounds: number): ResultRow => ({
    endpointName: endpoint.name.trim() || endpoint.baseUrl.trim() || "未命名",
    baseUrl: endpoint.baseUrl.trim(),
    model,
    rounds,
    status: "测试中",
    firstTokenLatencySecs: null,
    firstTokenMinusSecs: null,
    firstTokenPlusSecs: null,
    outputSpeedTps: null,
    outputMinusTps: null,
    outputPlusTps: null,
    success: null,
    successCount: 0,
    failCount: 0,
    failedRounds: [],
    roundResults: [],
    result: ""
  });

  let endpoints = $state<EndpointForm[]>([createEndpoint()]);
  let prompt = $state("请用两句话解释什么是性能测试。");
  let rounds = $state(1);
  let concurrency = $state(8);
  let maxTokens = $state(512);
  let temperature = $state(0);
  let userAgent = $state("silicon-perf/dev");
  let running = $state(false);
  let errorMessage = $state("");
  let rows = $state<ResultRow[]>([]);

  let detailOpen = $state(false);
  let detailTitle = $state("");
  let detailContent = $state("");
  let sortKey = $state<SortKey>("endpointName");
  let sortDirection = $state<SortDirection>("asc");

  let importInput: HTMLInputElement | null = null;

  function parseModels(input: string): string[] {
    return input
      .split(/[\n,]+/)
      .map((x: string) => x.trim())
      .filter(Boolean);
  }

  function addEndpoint() {
    endpoints = [...endpoints, createEndpoint()];
  }

  function removeEndpoint(index: number) {
    if (endpoints.length === 1) {
      return;
    }
    endpoints = endpoints.filter((_, i) => i !== index);
  }

  function updateEndpoint(index: number, field: keyof EndpointForm, value: string) {
    endpoints = endpoints.map((endpoint, i) => (i === index ? { ...endpoint, [field]: value } : endpoint));
  }

  function openDetail(row: ResultRow) {
    detailTitle = `${row.endpointName} / ${row.model} / ${row.rounds} 轮汇总`;
    detailContent = row.result || "(空)";
    detailOpen = true;
  }

  function closeDetail() {
    detailOpen = false;
  }

  function buildComboKey(endpointName: string, baseUrl: string, model: string) {
    return `${endpointName}|||${baseUrl}|||${model}`;
  }

  function summarizeMetric(values: number[]) {
    if (values.length === 0) {
      return { avg: null, minus: null, plus: null };
    }
    const total = values.reduce((acc, value) => acc + value, 0);
    const avg = total / values.length;
    const min = Math.min(...values);
    const max = Math.max(...values);
    return {
      avg,
      minus: min - avg,
      plus: max - avg
    };
  }

  function summarizeRowFromRounds(
    row: ResultRow,
    byRound: Map<number, BenchmarkResult>,
    totalRounds: number,
    finalizeMissing = false
  ): ResultRow {
    const items = Array.from(byRound.values())
      .filter((x) => x.round >= 1 && x.round <= totalRounds)
      .sort((a, b) => a.round - b.round);
    const successCount = items.filter((x) => x.success).length;
    const failCount = items.filter((x) => !x.success).length;
    const failedRounds = items.filter((x) => !x.success).map((x) => x.round);
    const doneCount = byRound.size;

    const firstValues = items
      .map((x) => x.firstTokenLatencySecs)
      .filter((x): x is number => typeof x === "number");
    const outputValues = items.map((x) => x.outputSpeedTps).filter((x): x is number => typeof x === "number");

    const firstSummary = summarizeMetric(firstValues);
    const outputSummary = summarizeMetric(outputValues);

    const mergedResult = Array.from({ length: totalRounds }, (_, idx) => {
      const round = idx + 1;
      const item = byRound.get(round);
      if (!item) {
        if (finalizeMissing) {
          return `【第${round}轮｜失败】\n未收到该轮结果（可能超时或事件丢失）`;
        }
        return `【第${round}轮｜测试中】\n等待结果...`;
      }
      return `【第${round}轮｜${item.status}】\n${item.result || "(空)"}`;
    }).join("\n\n-----\n\n");

    const missingCount = Math.max(0, totalRounds - doneCount);
    const mergedFailCount = failCount + (finalizeMissing ? missingCount : 0);
    const status: ResultRow["status"] =
      !finalizeMissing && doneCount < totalRounds ? "测试中" : mergedFailCount > 0 ? "失败" : "成功";

    return {
      ...row,
      status,
      success: !finalizeMissing && doneCount < totalRounds ? null : mergedFailCount === 0,
      successCount,
      failCount: mergedFailCount,
      failedRounds,
      roundResults: items,
      firstTokenLatencySecs: firstSummary.avg,
      firstTokenMinusSecs: firstSummary.minus,
      firstTokenPlusSecs: firstSummary.plus,
      outputSpeedTps: outputSummary.avg,
      outputMinusTps: outputSummary.minus,
      outputPlusTps: outputSummary.plus,
      result: mergedResult
    };
  }

  function getEndpointDisplayName(endpoint: EndpointRequest) {
    return endpoint.name.trim() || endpoint.baseUrl.trim() || "未命名";
  }

  function buildRunSettings(): RunSettings | null {
    const normalizedEndpoints: EndpointRequest[] = endpoints
      .map((item) => ({
        name: item.name.trim(),
        baseUrl: item.baseUrl.trim(),
        apiKey: item.apiKey.trim(),
        models: parseModels(item.modelsInput)
      }))
      .filter((item) => item.baseUrl && item.apiKey && item.models.length > 0);

    if (normalizedEndpoints.length === 0) {
      errorMessage = "请至少填写一组完整的 baseUrl + key + model。";
      return null;
    }

    if (!prompt.trim()) {
      errorMessage = "提示词不能为空。";
      return null;
    }

    return {
      normalizedEndpoints,
      safeRounds: Number(rounds) > 0 ? Number(rounds) : 1,
      safeConcurrency: Number(concurrency) > 0 ? Number(concurrency) : 1,
      safeMaxTokens: Number(maxTokens) > 0 ? Number(maxTokens) : 512
    };
  }

  async function runBenchmarkWithStream(
    normalizedEndpoints: EndpointRequest[],
    safeRounds: number,
    safeConcurrency: number,
    safeMaxTokens: number,
    retryRounds: number[] | null,
    onProgress: (item: BenchmarkResult) => void
  ): Promise<{ completed: number; total: number }> {
    let unlistenProgress: null | (() => void) = null;
    let latestProgress = { completed: 0, total: 0 };
    try {
      unlistenProgress = await listen<BenchmarkProgress>("benchmark-progress", (event) => {
        latestProgress = {
          completed: event.payload.completed,
          total: event.payload.total
        };
        onProgress(event.payload.item);
      });

      await invoke("run_benchmark", {
        request: {
          endpoints: normalizedEndpoints,
          prompt,
          rounds: safeRounds,
          concurrency: safeConcurrency,
          maxTokens: safeMaxTokens,
          temperature: Number(temperature),
          userAgent,
          retryRounds
        }
      });
      return latestProgress;
    } finally {
      if (unlistenProgress) {
        unlistenProgress();
      }
    }
  }

  async function retryRow(targetRow: ResultRow) {
    if (running) return;
    errorMessage = "";

    const settings = buildRunSettings();
    if (!settings) {
      return;
    }

    const { normalizedEndpoints, safeRounds, safeConcurrency, safeMaxTokens } = settings;
    const matchedEndpoint = normalizedEndpoints.find(
      (endpoint) =>
        endpoint.baseUrl === targetRow.baseUrl &&
        getEndpointDisplayName(endpoint) === targetRow.endpointName &&
        endpoint.models.includes(targetRow.model)
    );

    if (!matchedEndpoint) {
      errorMessage = "重试失败：当前配置中找不到该结果对应的 endpoint/model 组合。";
      return;
    }

    const retryEndpoint: EndpointRequest = {
      ...matchedEndpoint,
      models: [targetRow.model]
    };
    const rowKey = buildComboKey(targetRow.endpointName, targetRow.baseUrl, targetRow.model);
    const canRetryFailedOnly = targetRow.failCount > 0 && targetRow.failCount < targetRow.rounds;
    const retryFailedOnly =
      canRetryFailedOnly &&
      window.confirm(`检测到 ${targetRow.failCount} 个失败轮次，是否仅重试失败轮次？\n选择“取消”将重试全部轮次。`);
    const retryRounds = retryFailedOnly ? [...targetRow.failedRounds] : null;

    const groupedRounds = new Map<number, BenchmarkResult>();
    if (retryFailedOnly) {
      for (const item of targetRow.roundResults) {
        if (item.success && item.round <= safeRounds) {
          groupedRounds.set(item.round, item);
        }
      }
    }

    rows = rows.map((row) => {
      const key = buildComboKey(row.endpointName, row.baseUrl, row.model);
      if (key !== rowKey) {
        return row;
      }
      const reset = createRow(
        {
          name: row.endpointName,
          baseUrl: row.baseUrl,
          apiKey: matchedEndpoint.apiKey,
          models: [row.model]
        },
        row.model,
        safeRounds
      );
      return summarizeRowFromRounds(reset, groupedRounds, safeRounds, false);
    });

    running = true;
    try {
      const progress = await runBenchmarkWithStream(
        [retryEndpoint],
        safeRounds,
        safeConcurrency,
        safeMaxTokens,
        retryRounds,
        (item) => {
        const itemKey = buildComboKey(item.endpointName, item.baseUrl, item.model);
        if (itemKey !== rowKey) {
          return;
        }
        groupedRounds.set(item.round, item);
        rows = rows.map((row) => {
          const key = buildComboKey(row.endpointName, row.baseUrl, row.model);
          if (key !== rowKey) {
            return row;
          }
          return summarizeRowFromRounds(row, groupedRounds, safeRounds, false);
        });
        }
      );

      const expected = retryRounds?.length ?? safeRounds;
      const completed = Math.max(progress.completed, groupedRounds.size);
      const shouldFinalize = completed >= expected;

      rows = rows.map((row) => {
        const key = buildComboKey(row.endpointName, row.baseUrl, row.model);
        if (key !== rowKey) {
          return row;
        }
        return summarizeRowFromRounds(row, groupedRounds, safeRounds, shouldFinalize);
      });
    } catch (error: unknown) {
      const message = typeof error === "string" ? error : error instanceof Error ? error.message : "未知错误";
      errorMessage = `重试失败：${message}`;
    } finally {
      running = false;
    }
  }

  function toggleSort(key: SortKey) {
    if (sortKey === key) {
      sortDirection = sortDirection === "asc" ? "desc" : "asc";
      return;
    }
    sortKey = key;
    sortDirection = "asc";
  }

  function sortSymbol(key: SortKey) {
    if (sortKey !== key) return "";
    return sortDirection === "asc" ? "↑" : "↓";
  }

  function compareStatus(a: ResultRow["status"], b: ResultRow["status"]) {
    const rank = { "测试中": 0, 成功: 1, 失败: 2 };
    return rank[a] - rank[b];
  }

  function compareNullableNumber(a: number | null, b: number | null) {
    if (a == null && b == null) return 0;
    if (a == null) return 1;
    if (b == null) return -1;
    return a - b;
  }

  function getSortedRows() {
    const list = [...rows];
    const factor = sortDirection === "asc" ? 1 : -1;
    return list.sort((a, b) => {
      let delta = 0;
      switch (sortKey) {
        case "firstTokenLatencySecs":
          delta = compareNullableNumber(a.firstTokenLatencySecs, b.firstTokenLatencySecs);
          break;
        case "outputSpeedTps":
          delta = compareNullableNumber(a.outputSpeedTps, b.outputSpeedTps);
          break;
        case "status":
          delta = compareStatus(a.status, b.status);
          break;
        case "endpointName":
        case "model":
        case "result":
          delta = a[sortKey].localeCompare(b[sortKey], "zh-CN");
          break;
      }
      if (delta !== 0) {
        return delta * factor;
      }
      return a.model.localeCompare(b.model, "zh-CN");
    });
  }

  function formatLatency(avg: number | null, minus: number | null, plus: number | null) {
    if (avg == null) return "-";
    if (minus == null || plus == null) return `${avg.toFixed(2)} s`;
    return `${avg.toFixed(2)}(${minus.toFixed(2)},+${plus.toFixed(2)})s`;
  }

  function formatSpeed(avg: number | null, minus: number | null, plus: number | null) {
    if (avg == null) return "-";
    if (minus == null || plus == null) return `${avg.toFixed(1)} tok/s`;
    return `${avg.toFixed(1)}(${minus.toFixed(1)},+${plus.toFixed(1)})tok/s`;
  }

  async function exportConfig() {
    const payload: SavedConfig = {
      version: 1,
      endpoints,
      prompt,
      rounds: Number(rounds),
      concurrency: Number(concurrency),
      maxTokens: Number(maxTokens),
      temperature: Number(temperature),
      userAgent
    };

    try {
      const targetPath = await save({
        title: "导出测试配置",
        defaultPath: `silicon-perf-config-${Date.now()}.json`,
        filters: [{ name: "JSON", extensions: ["json"] }]
      });

      if (!targetPath) {
        return;
      }

      await invoke("save_config_file", {
        path: targetPath,
        content: JSON.stringify(payload, null, 2)
      });
      errorMessage = "";
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : "未知错误";
      errorMessage = `导出配置失败：${message}`;
    }
  }

  function triggerImportConfig() {
    importInput?.click();
  }

  async function importConfig(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;

    try {
      const raw = await file.text();
      const parsed = JSON.parse(raw) as Partial<SavedConfig>;
      if (!Array.isArray(parsed.endpoints) || parsed.endpoints.length === 0) {
        throw new Error("配置缺少 endpoints");
      }

      const importedEndpoints = parsed.endpoints
        .map((item) => ({
          name: typeof item.name === "string" ? item.name : "",
          baseUrl: typeof item.baseUrl === "string" ? item.baseUrl : "",
          apiKey: typeof item.apiKey === "string" ? item.apiKey : "",
          modelsInput: typeof item.modelsInput === "string" ? item.modelsInput : ""
        }))
        .filter((item) => item.baseUrl || item.apiKey || item.modelsInput || item.name);

      endpoints = importedEndpoints.length > 0 ? importedEndpoints : [createEndpoint()];
      if (typeof parsed.prompt === "string") prompt = parsed.prompt;
      if (typeof parsed.userAgent === "string") userAgent = parsed.userAgent;
      if (typeof parsed.rounds === "number") rounds = Math.max(1, Math.floor(parsed.rounds));
      if (typeof parsed.concurrency === "number") concurrency = Math.max(1, Math.floor(parsed.concurrency));
      if (typeof parsed.maxTokens === "number") maxTokens = Math.max(1, Math.floor(parsed.maxTokens));
      if (typeof parsed.temperature === "number") temperature = parsed.temperature;

      errorMessage = "";
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : "未知错误";
      errorMessage = `导入配置失败：${message}`;
    } finally {
      input.value = "";
    }
  }

  onMount(async () => {
    try {
      const fetchedUserAgent = await invoke<string>("get_default_user_agent");
      if (fetchedUserAgent?.trim()) {
        userAgent = fetchedUserAgent;
      }
    } catch {
      userAgent = "silicon-perf/dev";
    }
  });

  async function startBenchmark() {
    if (running) return;
    errorMessage = "";

    const settings = buildRunSettings();
    if (!settings) {
      return;
    }
    const { normalizedEndpoints, safeRounds, safeConcurrency, safeMaxTokens } = settings;

    const initialRows: ResultRow[] = [];
    for (const endpoint of normalizedEndpoints) {
      for (const model of endpoint.models) {
        initialRows.push(createRow(endpoint, model, safeRounds));
      }
    }
    rows = initialRows.map((row) => summarizeRowFromRounds(row, new Map<number, BenchmarkResult>(), safeRounds, false));

    running = true;
    try {
      const groupedRounds = new Map<string, Map<number, BenchmarkResult>>();
      const progress = await runBenchmarkWithStream(
        normalizedEndpoints,
        safeRounds,
        safeConcurrency,
        safeMaxTokens,
        null,
        (item) => {
        const key = buildComboKey(item.endpointName, item.baseUrl, item.model);
        const byRound = groupedRounds.get(key) ?? new Map<number, BenchmarkResult>();
        byRound.set(item.round, item);
        groupedRounds.set(key, byRound);

        rows = rows.map((row) => {
          const rowKey = buildComboKey(row.endpointName, row.baseUrl, row.model);
          if (rowKey !== key) {
            return row;
          }
          return summarizeRowFromRounds(row, byRound, safeRounds, false);
        });
        }
      );

      const shouldFinalize = progress.total > 0 && progress.completed >= progress.total;

      rows = rows.map((row) => {
        const key = buildComboKey(row.endpointName, row.baseUrl, row.model);
        const byRound = groupedRounds.get(key) ?? new Map<number, BenchmarkResult>();
        return summarizeRowFromRounds(row, byRound, safeRounds, shouldFinalize);
      });
    } catch (error: unknown) {
      const message =
        typeof error === "string"
          ? error
          : error instanceof Error
            ? error.message
            : "未知错误";
      errorMessage = `测试启动失败：${message}`;
      rows = rows.map((row) => {
        const failedResult = Array.from({ length: row.rounds }, (_, idx) => `【第${idx + 1}轮｜失败】\n${message}`).join(
          "\n\n-----\n\n"
        );
        return {
          ...row,
          status: "失败",
          success: false,
          failCount: row.rounds,
          failedRounds: Array.from({ length: row.rounds }, (_, idx) => idx + 1),
          roundResults: [],
          result: failedResult
        };
      });
    } finally {
      running = false;
    }
  }
</script>

<main class="app-shell">
  <header class="topbar panel">
    <p>silicon-perf · OpenAI 格式 · 桌面压测工具</p>
  </header>

  <div class="workspace">
    <section class="panel config-panel">
      <div class="title-row config-title-row">
        <h2>测试配置</h2>
        <div class="config-actions">
          <input bind:this={importInput} type="file" accept="application/json,.json" class="hidden" onchange={importConfig} />
          <button type="button" class="ghost" onclick={triggerImportConfig}>导入</button>
          <button type="button" class="ghost" onclick={exportConfig}>导出</button>
          <button type="button" class="ghost" onclick={addEndpoint}>+ Endpoint</button>
        </div>
      </div>

      <div class="endpoint-list">
        {#each endpoints as endpoint, index}
          <article class="endpoint-card">
            <div class="endpoint-head">
              <h3>Endpoint {index + 1}</h3>
              <button type="button" class="danger" disabled={endpoints.length === 1} onclick={() => removeEndpoint(index)}>
                删除
              </button>
            </div>
            <div class="grid two">
              <label>
                名称
                <input
                  placeholder="主线路 A"
                  value={endpoint.name}
                  oninput={(e) => updateEndpoint(index, "name", e.currentTarget.value)}
                />
              </label>
              <label>
                Base URL
                <input
                  placeholder="https://api.openai.com/v1"
                  value={endpoint.baseUrl}
                  oninput={(e) => updateEndpoint(index, "baseUrl", e.currentTarget.value)}
                />
              </label>
            </div>
            <div class="grid two">
              <label>
                API Key
                <input
                  type="password"
                  placeholder="sk-..."
                  value={endpoint.apiKey}
                  oninput={(e) => updateEndpoint(index, "apiKey", e.currentTarget.value)}
                />
              </label>
              <label>
                Models
                <textarea
                  rows="3"
                  placeholder="gpt-4o-mini, gpt-4.1-mini"
                  value={endpoint.modelsInput}
                  oninput={(e) => updateEndpoint(index, "modelsInput", e.currentTarget.value)}
                ></textarea>
              </label>
            </div>
          </article>
        {/each}
      </div>

      <div class="quick-grid">
        <label>
          轮次
          <input type="number" min="1" max="1000" bind:value={rounds} />
        </label>
        <label>
          并发
          <input type="number" min="1" max="256" bind:value={concurrency} />
        </label>
        <label>
          MaxTokens
          <input type="number" min="1" max="32768" bind:value={maxTokens} />
        </label>
        <label>
          Temperature
          <input type="number" min="0" max="2" step="0.1" bind:value={temperature} />
        </label>
      </div>

      <label>
        User-Agent
        <input placeholder="silicon-perf/0.1.0" bind:value={userAgent} />
      </label>

      <label class="prompt-box">
        提示词
        <textarea rows="4" bind:value={prompt} placeholder="请输入用于测试的提示词"></textarea>
      </label>

      <div class="action-row">
        <button type="button" class="primary" disabled={running} onclick={startBenchmark}>
          {running ? "测试中..." : "开始测试"}
        </button>
        {#if errorMessage}
          <span class="error">{errorMessage}</span>
        {/if}
      </div>
    </section>

    <section class="panel result-panel">
      <div class="title-row result-title-row">
        <h2>测试结果</h2>
        <span class="meta">共 {rows.length} 条</span>
      </div>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th><button type="button" class="th-btn" onclick={() => toggleSort("endpointName")}>Endpoint {sortSymbol("endpointName")}</button></th>
              <th><button type="button" class="th-btn" onclick={() => toggleSort("model")}>Model {sortSymbol("model")}</button></th>
              <th>
                <button type="button" class="th-btn" onclick={() => toggleSort("firstTokenLatencySecs")}>
                  首字 {sortSymbol("firstTokenLatencySecs")}
                </button>
              </th>
              <th><button type="button" class="th-btn" onclick={() => toggleSort("outputSpeedTps")}>输出 {sortSymbol("outputSpeedTps")}</button></th>
              <th><button type="button" class="th-btn" onclick={() => toggleSort("result")}>结果 {sortSymbol("result")}</button></th>
              <th><button type="button" class="th-btn" onclick={() => toggleSort("status")}>状态 {sortSymbol("status")}</button></th>
            </tr>
          </thead>
          <tbody>
            {#if rows.length === 0}
              <tr>
                <td colspan="6" class="empty">暂无结果，先配置后开始测试。</td>
              </tr>
            {:else}
              {#each getSortedRows() as row}
                <tr>
                  <td>{row.endpointName}</td>
                  <td>{row.model}</td>
                  <td>{formatLatency(row.firstTokenLatencySecs, row.firstTokenMinusSecs, row.firstTokenPlusSecs)}</td>
                  <td>{formatSpeed(row.outputSpeedTps, row.outputMinusTps, row.outputPlusTps)}</td>
                  <td>
                    <div class="cell-actions">
                      <button type="button" class="link" onclick={() => openDetail(row)}>查看</button>
                      <button type="button" class="link" disabled={running} onclick={() => retryRow(row)}>重试</button>
                    </div>
                  </td>
                  <td>
                    <span class={`badge ${row.status === "成功" ? "ok" : row.status === "失败" ? "fail" : "running"}`}>
                      {row.status} ({row.successCount}/{row.rounds})
                    </span>
                  </td>
                </tr>
              {/each}
            {/if}
          </tbody>
        </table>
      </div>
    </section>
  </div>

  {#if detailOpen}
    <div class="dialog-mask" role="button" tabindex="0" onkeydown={closeDetail} onclick={closeDetail}>
      <div
        class="dialog"
        role="dialog"
        aria-modal="true"
        tabindex="-1"
        onclick={(e) => e.stopPropagation()}
        onkeydown={(e) => e.stopPropagation()}
      >
        <header>
          <h3>{detailTitle}</h3>
          <button type="button" class="ghost" onclick={closeDetail}>关闭</button>
        </header>
        <pre>{detailContent}</pre>
      </div>
    </div>
  {/if}
</main>

<style>
  :global(html),
  :global(body) {
    height: 100%;
    margin: 0;
    font-family: "Noto Sans SC", "PingFang SC", "Microsoft YaHei", sans-serif;
    background:
      radial-gradient(circle at 15% 0%, #ffe3b5 0, transparent 35%),
      radial-gradient(circle at 90% 15%, #b6dde8 0, transparent 40%),
      linear-gradient(145deg, #faf6ef 0%, #f4f8fc 100%);
    color: #2f2a22;
    overflow: hidden;
  }

  .app-shell {
    box-sizing: border-box;
    height: 100vh;
    padding: 10px;
    display: grid;
    grid-template-rows: auto 1fr;
    gap: 8px;
    min-height: 0;
  }

  .topbar {
    display: flex;
    align-items: center;
    padding: 8px 10px;
  }

  .topbar p {
    margin: 0;
    color: #65553d;
    font-size: 12px;
  }

  .workspace {
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(370px, 470px) minmax(0, 1fr);
    gap: 8px;
  }

  .panel {
    background: rgba(255, 255, 255, 0.86);
    border: 1px solid #e5dcca;
    border-radius: 10px;
    box-shadow: 0 4px 10px rgba(92, 80, 62, 0.06);
    padding: 10px;
    min-height: 0;
  }

  .config-panel,
  .result-panel {
    display: grid;
    grid-template-rows: auto auto auto auto auto auto;
    gap: 8px;
    overflow: hidden;
  }

  .result-panel {
    grid-template-rows: auto 1fr;
  }

  .meta {
    font-size: 12px;
    color: #7f6d54;
  }

  .title-row,
  .endpoint-head,
  .action-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
  }

  .config-title-row {
    gap: 6px;
    margin-bottom: -2px;
  }

  .config-title-row :global(button) {
    padding-top: 4px;
    padding-bottom: 4px;
  }

  .result-title-row {
    min-height: 24px;
  }

  h2 {
    margin: 0;
    font-size: 14px;
    line-height: 1.2;
  }

  h3 {
    margin: 0;
    font-size: 12px;
    line-height: 1.2;
    font-weight: 700;
  }

  .config-actions {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .hidden {
    display: none;
  }

  .endpoint-list {
    overflow: auto;
    min-height: 140px;
    max-height: 44vh;
    display: grid;
    gap: 8px;
    padding-right: 2px;
  }

  .endpoint-card {
    border: 1px solid #e8dfcf;
    border-radius: 8px;
    padding: 8px;
    background: #fffdf9;
    display: grid;
    gap: 8px;
  }

  .grid {
    display: grid;
    gap: 8px;
  }

  .grid.two {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .quick-grid {
    display: grid;
    gap: 8px;
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  label {
    display: grid;
    gap: 4px;
    color: #5d4e39;
    font-size: 12px;
  }

  input,
  textarea,
  button {
    font: inherit;
    border-radius: 10px;
  }

  input,
  textarea {
    border: 1px solid #d8cfbe;
    background: #ffffff;
    padding: 6px 8px;
    color: #2f2a22;
    font-size: 13px;
  }

  textarea {
    resize: vertical;
  }

  .prompt-box textarea {
    min-height: 76px;
    max-height: 20vh;
  }

  button {
    border: 1px solid transparent;
    padding: 6px 10px;
    cursor: pointer;
    font-size: 12px;
    line-height: 1.2;
  }

  button:disabled {
    cursor: not-allowed;
    opacity: 0.6;
  }

  .primary {
    background: linear-gradient(120deg, #146f66 0%, #2a8f83 100%);
    color: #fff;
  }

  .ghost {
    border-color: #ccbfa7;
    background: #f7f2e8;
    color: #5e4e36;
  }

  .danger {
    border-color: #d8a9a1;
    background: #fbefed;
    color: #8c3b2d;
  }

  .error {
    color: #a53321;
    font-size: 12px;
  }

  .table-wrap {
    overflow: auto both;
    border: 1px solid #e2d8c6;
    border-radius: 8px;
    background: #fff;
    min-height: 0;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    min-width: 760px;
  }

  th,
  td {
    padding: 7px 8px;
    border-bottom: 1px solid #eee5d5;
    text-align: left;
    font-size: 12px;
    line-height: 1.25;
  }

  th {
    background: #f7f2e8;
    color: #5b4b36;
  }

  .th-btn {
    border: none;
    background: transparent;
    color: inherit;
    font: inherit;
    font-weight: 700;
    padding: 0;
    cursor: pointer;
    text-align: left;
    width: 100%;
  }

  .empty {
    text-align: center;
    color: #8d7d67;
  }

  .link {
    border: none;
    background: none;
    color: #0b6b9a;
    padding: 0;
    text-decoration: underline;
  }

  .badge {
    display: inline-block;
    border-radius: 999px;
    padding: 2px 8px;
    font-size: 11px;
    border: 1px solid transparent;
  }

  .badge.running {
    background: #fff4de;
    border-color: #f0d18f;
    color: #935f00;
  }

  .badge.ok {
    background: #e9f7ec;
    border-color: #9ed4a7;
    color: #1f6a2f;
  }

  .badge.fail {
    background: #fdeeee;
    border-color: #e2b2b2;
    color: #8c2b2b;
  }

  .dialog-mask {
    position: fixed;
    inset: 0;
    background: rgba(45, 36, 21, 0.45);
    display: grid;
    place-items: center;
    padding: 12px;
  }

  .dialog {
    width: min(900px, 94vw);
    max-height: 82vh;
    background: #fff;
    border-radius: 8px;
    border: 1px solid #e6dcc8;
    overflow: hidden;
    display: grid;
    grid-template-rows: auto 1fr;
  }

  .dialog header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 8px;
    border-bottom: 1px solid #ede4d3;
  }

  .dialog h3 {
    margin: 0;
    font-size: 14px;
    color: #4f3f28;
  }

  .dialog pre {
    margin: 0;
    padding: 8px;
    overflow: auto;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: "Cascadia Code", "Consolas", monospace;
    font-size: 12px;
    line-height: 1.5;
    color: #2f2a22;
  }

  @media (max-width: 840px) {
    :global(body) {
      overflow: auto;
    }

    .app-shell {
      height: auto;
      min-height: 100vh;
    }

    .workspace {
      grid-template-columns: 1fr;
      grid-template-rows: auto auto;
    }

    .config-panel,
    .result-panel {
      overflow: visible;
    }

    .endpoint-list {
      max-height: none;
      overflow: visible;
    }

    .grid.two,
    .quick-grid {
      grid-template-columns: 1fr;
    }

    .title-row,
    .endpoint-head,
    .action-row {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
