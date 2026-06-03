const state = {
  activeJobId: null,
  pollTimer: null,
  currentTab: "syscall",
  knowledgeGraph: null,
  selectedKnowledgeNode: null,
};

const $ = (id) => document.getElementById(id);

async function api(path, options = {}) {
  const response = await fetch(path, {
    headers: { "Content-Type": "application/json" },
    ...options,
  });
  const payload = await response.json();
  if (!response.ok) {
    throw new Error(payload.error || `${response.status} ${response.statusText}`);
  }
  return payload;
}

function selectValue(id) {
  return $(id).value;
}

function numberValue(id) {
  return Number($(id).value);
}

function checked(id) {
  return $(id).checked;
}

function textValue(id) {
  return $(id).value.trim();
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function jsonCell(value) {
  if (value === null || value === undefined) {
    return '<span class="muted">missing</span>';
  }
  return `<code>${escapeHtml(JSON.stringify(value))}</code>`;
}

function setBusy(isBusy) {
  document.querySelectorAll("button").forEach((button) => {
    if (button.id !== "refresh-job" && button.id !== "refresh-all") {
      button.disabled = isBusy;
    }
  });
}

function setActiveState(text, status = "idle") {
  const pill = $("active-state");
  pill.textContent = text;
  pill.className = `status-pill ${status}`;
}

function summaryItem(label, value, className = "") {
  return `
    <div class="summary-item">
      <div class="summary-label">${escapeHtml(label)}</div>
      <div class="summary-value ${className}">${escapeHtml(value)}</div>
    </div>`;
}

const SAMPLE_METRICS = [
  { label: "Samples", keys: ["samples", "total_samples", "folded_stack_lines", "sample_count"] },
  { label: "Dropped", keys: ["dropped", "dropped_samples", "samples_dropped", "dropped_count"] },
  { label: "Sample failures", keys: ["sample_failures", "sample_failures_total", "failed_samples", "failures"] },
];

const PLUGIN_COUNTER_METRICS = [
  ...SAMPLE_METRICS,
  { label: "Exec insns", keys: ["executed_instructions"] },
  { label: "Exec blocks", keys: ["executed_blocks"] },
  { label: "Translated insns", keys: ["translated_instructions"] },
  { label: "Translated blocks", keys: ["translated_blocks"] },
  { label: "Callbacks", keys: ["execute_callbacks"] },
];

const HOST_METRICS = [
  ...SAMPLE_METRICS,
  { label: "Elapsed", keys: ["elapsed", "elapsed_sec", "elapsed_seconds", "elapsed_s", "wall_time", "wall_time_sec"] },
  { label: "User", keys: ["user", "user_sec", "user_seconds", "user_time"] },
  { label: "Sys", keys: ["sys", "sys_sec", "sys_seconds", "system", "system_sec", "system_time"] },
  { label: "Max RSS", keys: ["max_rss", "max_rss_kb", "maximum_resident_set_size"] },
];

const HOST_PERF_METRICS = [
  ...SAMPLE_METRICS,
  { label: "Cycles", keys: ["cycles", "cpu-cycles", "cpu_cycles"] },
  { label: "Instructions", keys: ["instructions"] },
  { label: "Cache misses", keys: ["cache-misses", "cache_misses"] },
  { label: "Task clock", keys: ["task-clock", "task_clock", "task_clock_ms"] },
];

const EXTRA_METRIC_LABELS = {
  build_profile: "Build profile",
  flamegraph_generated: "Flamegraph",
  folded_stack_lines: "Samples",
  frequency_hz: "Frequency",
  kernel_filter: "Kernel filter",
  max_rss: "Max RSS",
  max_rss_kb: "Max RSS",
  max_stack_depth: "Max depth",
  plugin_summary: "Plugin summary",
  qperf_format_version: "qperf format",
  queue_size: "Queue size",
  sampling_mode: "Mode",
  timeout_seconds: "Timeout",
  translated_blocks: "Translated blocks",
  translated_instructions: "Translated insns",
  executed_blocks: "Executed blocks",
  executed_instructions: "Executed insns",
  execute_callbacks: "Callbacks",
};

function metricObject(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function flattenPerfEvents(source) {
  const object = metricObject(source);
  if (!object) {
    return null;
  }
  const flattened = {};
  for (const [name, event] of Object.entries(object.events || {})) {
    if (event && typeof event === "object" && event.value !== null && event.value !== undefined) {
      flattened[name] = event.unit ? `${event.value} ${event.unit}` : event.value;
    }
  }
  if (object.scope) {
    flattened.scope = object.scope;
  }
  if (object.note) {
    flattened.note = object.note;
  }
  if (Array.isArray(object.errors) && object.errors.length) {
    flattened.errors = object.errors.join("; ");
  }
  return Object.keys(flattened).length ? flattened : object;
}

function metricLookup(source, keys) {
  if (!source) {
    return null;
  }
  for (const key of keys) {
    if (
      Object.prototype.hasOwnProperty.call(source, key) &&
      source[key] !== null &&
      source[key] !== undefined &&
      source[key] !== ""
    ) {
      return { key, value: source[key] };
    }
  }
  return null;
}

function firstMetric(sources, keys) {
  for (const source of sources) {
    const match = metricLookup(metricObject(source), keys);
    if (match) {
      return match.value;
    }
  }
  return null;
}

function formatMetricValue(value) {
  if (typeof value === "number") {
    return Number.isInteger(value)
      ? value.toLocaleString()
      : value.toLocaleString(undefined, { maximumFractionDigits: 3 });
  }
  if (typeof value === "boolean") {
    return value ? "true" : "false";
  }
  if (Array.isArray(value)) {
    return value.join(", ");
  }
  if (value && typeof value === "object") {
    return JSON.stringify(value);
  }
  return String(value);
}

function humanMetricLabel(key) {
  return EXTRA_METRIC_LABELS[key] || key.replaceAll("_", " ").replaceAll("-", " ");
}

function isExtraMetric(key, value) {
  if (value === null || value === undefined || value === "" || typeof value === "object") {
    return false;
  }
  const lower = key.toLowerCase();
  return !(
    lower.includes("path") ||
    lower.endsWith("_dir") ||
    ["analyzer", "flamegraph", "folded_stack", "kernel_elf", "plugin", "raw_samples"].includes(lower)
  );
}

async function refreshStatus() {
  const status = await api("/api/status");
  $("repo-root").textContent = status.repo_root;
  const kgRoot = $("kg-repo-root");
  if (kgRoot && (!kgRoot.value || kgRoot.value === "/home/cg24/tg-arceos-tutorial")) {
    kgRoot.value = status.repo_root;
  }
  if (status.active_job) {
    state.activeJobId = status.active_job.id;
    setActiveState(`${status.active_job.kind}: ${status.active_job.status}`, "running");
    setBusy(true);
    schedulePoll();
  } else {
    setActiveState("idle");
    setBusy(false);
  }
  return status;
}

async function loadReport(kind, arch) {
  const query = new URLSearchParams({ kind });
  if (arch) {
    query.set("arch", arch);
  }
  return api(`/api/report?${query.toString()}`);
}

async function refreshReports() {
  await Promise.allSettled([refreshSyscallReport(), refreshPerfReport(), refreshDiffReport()]);
}

async function refreshKnowledgeGraph(refresh = false) {
  try {
    $("kg-meta").textContent = "scanning repository";
    const query = new URLSearchParams({
      task: textValue("kg-task"),
      repo_root: textValue("kg-repo-root"),
      granularity: selectValue("kg-granularity"),
      refresh: refresh ? "1" : "0",
    });
    const graph = await api(`/api/knowledge-graph?${query.toString()}`);
    state.knowledgeGraph = graph;
    state.selectedKnowledgeNode = null;
    renderKnowledgeGraph(graph);
  } catch (error) {
    $("kg-meta").textContent = `scan failed: ${error}`;
    $("kg-summary").innerHTML = summaryItem("Graph", "failed", "bad");
    $("kg-graph").innerHTML = "";
    $("kg-detail-title").textContent = "OS 知识图谱";
    $("kg-detail-body").innerHTML = `<div class="brief-text bad">${escapeHtml(String(error))}</div>`;
  }
}

async function refreshSyscallReport() {
  try {
    const report = await loadReport("syscall", selectValue("syscall-arch"));
    renderSyscall(report);
  } catch {
    $("syscall-report-path").textContent = "no report";
    $("syscall-summary").innerHTML = summaryItem("Diffs", "none");
    $("syscall-diffs").innerHTML = emptyRow(3, "no syscall report");
    $("syscall-brief").innerHTML = '<div class="brief-text">no repair context</div>';
  }
}

function renderSyscall(report) {
  const differences = report.differences || [];
  $("syscall-report-path").textContent = report._ui.report_path;
  $("syscall-summary").innerHTML = [
    summaryItem("Arch", report.arch || "unknown"),
    summaryItem("Diffs", differences.length, differences.length ? "bad" : "good"),
    summaryItem("Begin marker", report.markers?.starry_begin ? "yes" : "no"),
    summaryItem("End marker", report.markers?.starry_end ? "yes" : "no"),
  ].join("");
  $("syscall-diffs").innerHTML = differences.length
    ? differences
        .map(
          (diff) => `
            <tr>
              <td><code>${escapeHtml(diff.case)}</code></td>
              <td>${jsonCell(diff.linux)}</td>
              <td>${jsonCell(diff.starry)}</td>
            </tr>`,
        )
        .join("")
    : emptyRow(3, "no semantic differences");
  $("syscall-brief").innerHTML = renderSyscallBrief(differences);
}

function renderSyscallBrief(differences) {
  if (!differences.length) {
    return '<div class="brief-text good">Linux 对拍未发现语义差异。</div>';
  }
  return differences
    .map(
      (diff) => `
        <div class="brief-block">
          <div class="brief-title"><code>${escapeHtml(diff.case)}</code></div>
          <div class="brief-text">以 Linux 字段为准修正 StarryOS 返回值、errno 或状态变更；优先检查对应 syscall 实现和参数校验路径。</div>
        </div>`,
    )
    .join("");
}

async function refreshPerfReport() {
  try {
    const report = await loadReport("perf", selectValue("perf-arch"));
    renderPerf(report);
  } catch {
    $("perf-report-path").textContent = "no report";
    $("perf-summary").innerHTML = summaryItem("Samples", "none");
    $("perf-metrics").innerHTML = "";
    $("perf-functions").innerHTML = emptyRow(3, "no qperf report");
    $("perf-candidates").innerHTML = '<div class="brief-text">no candidates</div>';
    setFlamegraph(null, "no qperf report");
  }
}

function renderPerf(report) {
  const hotspots = report.hotspots || {};
  const functions = hotspots.top_functions || [];
  const samples = firstMetric(
    [hotspots, report.plugin_summary, report.summary, report.host_time_metrics, report.host_perf_metrics],
    ["total_samples", "samples", "folded_stack_lines", "sample_count"],
  );
  $("perf-report-path").textContent = report._ui.report_path;
  $("perf-summary").innerHTML = [
    summaryItem("Arch", report.arch || "unknown"),
    summaryItem("Result", report.result || "unknown", report.result === "ok" ? "good" : "warn"),
    summaryItem("Samples", samples ?? 0),
    summaryItem("Candidates", (report.fix_candidates || []).length),
  ].join("");
  $("perf-metrics").innerHTML = renderPerfMetrics(report);
  $("perf-functions").innerHTML = functions.length
    ? functions
        .map(
          (item) => `
            <tr>
              <td><code>${escapeHtml(item.function)}</code></td>
              <td>${escapeHtml(item.samples)}</td>
              <td>${escapeHtml(Number(item.percent).toFixed(2))}%</td>
            </tr>`,
        )
        .join("")
    : emptyRow(3, "no hotspots");
  $("perf-candidates").innerHTML = renderPerfCandidates(report.fix_candidates || []);
  const flamegraph = report._ui.artifacts?.flamegraph;
  setFlamegraph(
    flamegraph?.exists ? flamegraph.url : null,
    flamegraphMessage(report, flamegraph),
  );
}

function renderPerfMetrics(report) {
  return [
    renderMetricGroup("Summary", metricObject(report.summary), SAMPLE_METRICS),
    renderMetricGroup("Guest counters", metricObject(report.plugin_summary), PLUGIN_COUNTER_METRICS),
    renderMetricGroup("Host time", metricObject(report.host_time_metrics), HOST_METRICS),
    renderMetricGroup("Host perf", flattenPerfEvents(report.host_perf_metrics), HOST_PERF_METRICS),
  ]
    .filter(Boolean)
    .join("");
}

function renderMetricGroup(title, source, specs) {
  if (!source) {
    return "";
  }
  const rows = [];
  const usedKeys = new Set();
  for (const spec of specs) {
    const match = metricLookup(source, spec.keys);
    if (match) {
      usedKeys.add(match.key);
      rows.push([spec.label, match.value]);
    }
  }
  for (const [key, value] of Object.entries(source)) {
    if (rows.length >= 12) {
      break;
    }
    if (usedKeys.has(key) || !isExtraMetric(key, value)) {
      continue;
    }
    rows.push([humanMetricLabel(key), value]);
  }
  if (!rows.length) {
    return "";
  }
  return `
    <section class="metric-group">
      <h3>${escapeHtml(title)}</h3>
      ${rows
        .map(
          ([label, value]) => `
            <div class="metric-row">
              <span class="metric-label">${escapeHtml(label)}</span>
              <span class="metric-value">${escapeHtml(formatMetricValue(value))}</span>
            </div>`,
        )
        .join("")}
    </section>`;
}

function renderPerfCandidates(candidates) {
  if (!candidates.length) {
    return '<div class="brief-text">没有超过阈值的规则候选。</div>';
  }
  return candidates
    .map(
      (candidate) => `
        <div class="brief-block">
          <div class="brief-title">${escapeHtml(candidate.id)} · ${Number(candidate.percent).toFixed(2)}%</div>
          <div class="brief-text">trigger: <code>${escapeHtml(candidate.trigger)}</code></div>
          <div class="brief-text">${escapeHtml(candidate.strategy)}</div>
          <div class="brief-text">${escapeHtml((candidate.files || []).join(", "))}</div>
        </div>`,
    )
    .join("");
}

function flamegraphMessage(report, flamegraph) {
  const format = report.parameters?.format || "unknown";
  if (format === "folded") {
    return "format folded did not request SVG";
  }
  if (flamegraph && !flamegraph.exists) {
    return "flamegraph.svg was not generated; check profile.stderr";
  }
  return "no flamegraph";
}

function setFlamegraph(url, message = "no flamegraph") {
  const frame = $("flamegraph-frame");
  const empty = $("flamegraph-empty");
  if (!url) {
    frame.hidden = true;
    frame.removeAttribute("src");
    frame.style.width = "";
    frame.style.height = "";
    empty.textContent = message;
    empty.hidden = false;
    return;
  }
  frame.onload = resizeFlamegraphFrame;
  frame.hidden = false;
  frame.src = url;
  empty.hidden = true;
}

function resizeFlamegraphFrame() {
  const frame = $("flamegraph-frame");
  const svg = frame.contentDocument?.documentElement;
  const width = Number(svg?.getAttribute("width"));
  const height = Number(svg?.getAttribute("height"));
  if (Number.isFinite(width) && width > 0) {
    frame.style.width = `${Math.max(width, 1600)}px`;
  }
  if (Number.isFinite(height) && height > 0) {
    frame.style.height = `${Math.max(height + 20, 220)}px`;
  }
}

async function refreshDiffReport() {
  try {
    const report = await loadReport("perf-diff", null);
    renderDiff(report);
  } catch {
    $("diff-report-path").textContent = "no report";
    $("diff-changes").innerHTML = emptyRow(4, "no diff report");
  }
}

function renderDiff(report) {
  $("diff-report-path").textContent = report._ui.report_path;
  const changes = report.top_changes || [];
  $("diff-changes").innerHTML = changes.length
    ? changes
        .map(
          (item) => `
            <tr>
              <td><code>${escapeHtml(item.function)}</code></td>
              <td>${escapeHtml(Number(item.baseline_percent).toFixed(2))}%</td>
              <td>${escapeHtml(Number(item.compare_percent).toFixed(2))}%</td>
              <td class="${item.delta_percent > 0 ? "bad" : item.delta_percent < 0 ? "good" : ""}">
                ${escapeHtml(Number(item.delta_percent).toFixed(2))}%
              </td>
            </tr>`,
        )
        .join("")
    : emptyRow(4, "no changes");
}

function renderKnowledgeGraph(graph) {
  const allNodes = graph.graph?.nodes || [];
  const focusIds = new Set(graph.task?.focus_node_ids || []);
  const nodes = checked("kg-hide-empty")
    ? allNodes.filter((node) => focusIds.has(node.id) || Number(node.stats?.files || 0) > 0)
    : allNodes;
  const visibleIds = new Set(nodes.map((node) => node.id));
  const edges = (graph.graph?.edges || []).filter((edge) => visibleIds.has(edge.source) && visibleIds.has(edge.target));
  $("kg-meta").textContent = `扫描仓库：${graph.repo_root} · ${new Date(graph.generated_at * 1000).toLocaleString()}`;
  $("kg-summary").innerHTML = [
    summaryItem("Repo", graph.repo_root?.split("/").filter(Boolean).at(-1) || "unknown"),
    summaryItem("Nodes", nodes.length),
    summaryItem("Edges", edges.length),
    summaryItem("Scanned files", graph.scan?.files_seen ?? 0),
    summaryItem("Focus", (graph.task?.focus_node_ids || []).join(", ") || "auto"),
  ].join("");
  drawKnowledgeSvg(nodes, edges, focusIds);
  renderKnowledgeFocus(graph);
}

function drawKnowledgeSvg(nodes, edges, focusIds) {
  const svg = $("kg-graph");
  svg.innerHTML = "";
  const width = 1180;
  const layers = [...new Set(nodes.map((node) => node.layer))];
  const rowHeight = 132;
  const height = Math.max(420, layers.length * rowHeight + 50);
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
  svg.setAttribute("width", width);
  svg.setAttribute("height", height);

  const byLayer = new Map(layers.map((layer) => [layer, nodes.filter((node) => node.layer === layer)]));
  const positions = new Map();
  layers.forEach((layer, row) => {
    const layerNodes = byLayer.get(layer) || [];
    const gap = width / (layerNodes.length + 1);
    layerNodes.forEach((node, index) => {
      positions.set(node.id, { x: Math.round(gap * (index + 1)), y: 48 + row * rowHeight });
    });
  });

  const graphLayer = svgEl("g");
  svg.appendChild(graphLayer);
  for (const edge of edges) {
    const source = positions.get(edge.source);
    const target = positions.get(edge.target);
    if (!source || !target) {
      continue;
    }
    const line = svgEl("line", {
      x1: source.x,
      y1: source.y + 18,
      x2: target.x,
      y2: target.y - 18,
      class: edge.focus ? "kg-edge focus" : "kg-edge",
    });
    graphLayer.appendChild(line);
  }

  layers.forEach((layer, row) => {
    const label = svgEl("text", { x: 14, y: 52 + row * rowHeight, class: "kg-layer-label" });
    label.textContent = layer;
    graphLayer.appendChild(label);
  });

  for (const node of nodes) {
    const pos = positions.get(node.id);
    if (!pos) {
      continue;
    }
    const group = svgEl("g", {
      class: [
        "kg-node",
        focusIds.has(node.id) ? "focus" : "",
        state.selectedKnowledgeNode === node.id ? "selected" : "",
      ].join(" "),
      tabindex: "0",
    });
    group.addEventListener("click", () => selectKnowledgeNode(node.id));
    group.addEventListener("keydown", (event) => {
      if (event.key === "Enter" || event.key === " ") {
        selectKnowledgeNode(node.id);
      }
    });
    group.appendChild(svgEl("rect", { x: pos.x - 70, y: pos.y - 28, width: 140, height: 56, rx: 8 }));
    const title = svgEl("text", { x: pos.x, y: pos.y - 4, class: "kg-node-title" });
    title.textContent = shortNodeLabel(node.label);
    const meta = svgEl("text", { x: pos.x, y: pos.y + 16, class: "kg-node-meta" });
    meta.textContent = `${node.stats?.files ?? 0} files · ${node.task_score ?? 0}`;
    group.appendChild(title);
    group.appendChild(meta);
    graphLayer.appendChild(group);
  }
}

function svgEl(name, attributes = {}) {
  const element = document.createElementNS("http://www.w3.org/2000/svg", name);
  for (const [key, value] of Object.entries(attributes)) {
    element.setAttribute(key, String(value));
  }
  return element;
}

function shortNodeLabel(label) {
  const compact = String(label).replace("Linux ", "").replace(" driver", "").replace(" / ", "/");
  return compact.length > 23 ? `${compact.slice(0, 21)}…` : compact;
}

function selectKnowledgeNode(nodeId) {
  state.selectedKnowledgeNode = nodeId;
  const graph = state.knowledgeGraph;
  if (!graph) {
    return;
  }
  drawKnowledgeSvg(graph.graph?.nodes || [], graph.graph?.edges || [], new Set(graph.task?.focus_node_ids || []));
  const node = (graph.graph?.nodes || []).find((item) => item.id === nodeId);
  if (node) {
    renderKnowledgeNodeDetail(node);
  }
}

function renderKnowledgeFocus(graph) {
  $("kg-detail-title").textContent = "任务焦点";
  const focusNodes = new Set(graph.task?.focus_node_ids || []);
  const focusLabels = (graph.graph?.nodes || [])
    .filter((node) => focusNodes.has(node.id))
    .map((node) => node.label);
  $("kg-detail-body").innerHTML = [
    `<div class="brief-title">${escapeHtml(graph.focus?.summary || "未识别任务焦点")}</div>`,
    `<div class="brief-text">焦点节点：${escapeHtml(focusLabels.join("、") || "无")}</div>`,
    renderTextBlockList("代码讲解", graph.focus?.code_explanation || []),
    renderTextBlockList("OS 课本知识 - 实践关联", graph.focus?.os_explanation || []),
    renderTextBlockList("编码建议", graph.focus?.coding_guidance || []),
  ].join("");
}

function renderKnowledgeNodeDetail(node) {
  $("kg-detail-title").textContent = node.label;
  const hotFiles = (node.hot_files || [])
    .map((item) => `${item.path}${item.lines ? `:${item.lines} lines` : ""}`)
    .join("\n");
  const symbols = (node.symbols || [])
    .map((item) => `${item.kind} ${item.name} · ${item.path}:${item.line}`)
    .join("\n");
  $("kg-detail-body").innerHTML = [
    `<div class="brief-title">${escapeHtml(node.layer)} · score ${escapeHtml(node.task_score ?? 0)}</div>`,
    `<div class="brief-text">${escapeHtml(node.summary)}</div>`,
    renderMetricInline(node.stats || {}),
    renderCodeBlock("相关目录", (node.paths || []).join("\n")),
    renderCodeBlock("代表文件", hotFiles || "no files"),
    renderCodeBlock("代表符号", symbols || "no symbols"),
    `<div class="brief-block"><div class="brief-title">OS 课本知识</div><div class="brief-text">${escapeHtml(node.textbook)}</div></div>`,
    `<div class="brief-block"><div class="brief-title">仓库实践关联</div><div class="brief-text">${escapeHtml(node.practice)}</div></div>`,
  ].join("");
}

function renderMetricInline(stats) {
  return `
    <div class="kg-inline-stats">
      <span>files ${escapeHtml(stats.files ?? 0)}</span>
      <span>lines ${escapeHtml(stats.lines ?? 0)}</span>
      <span>symbols ${escapeHtml(stats.symbols ?? 0)}</span>
      <span>keyword hits ${escapeHtml(stats.keyword_hits ?? 0)}</span>
    </div>`;
}

function renderTextBlockList(title, items) {
  if (!items.length) {
    return "";
  }
  return `
    <div class="brief-block">
      <div class="brief-title">${escapeHtml(title)}</div>
      ${items.map((item) => `<div class="brief-text">${escapeHtml(item)}</div>`).join("")}
    </div>`;
}

function renderCodeBlock(title, value) {
  return `
    <div class="brief-block">
      <div class="brief-title">${escapeHtml(title)}</div>
      <pre class="kg-code">${escapeHtml(value)}</pre>
    </div>`;
}

function emptyRow(colspan, text) {
  return `<tr><td class="muted" colspan="${colspan}">${escapeHtml(text)}</td></tr>`;
}

async function startJob(kind, payload = {}) {
  const job = await api("/api/jobs", {
    method: "POST",
    body: JSON.stringify({ kind, ...payload }),
  });
  state.activeJobId = job.id;
  renderJob(job);
  setActiveState(`${job.kind}: ${job.status}`, "running");
  setBusy(true);
  showTab("jobs");
  schedulePoll();
}

function schedulePoll() {
  if (state.pollTimer) {
    return;
  }
  state.pollTimer = window.setInterval(pollJob, 1000);
}

async function pollJob() {
  if (!state.activeJobId) {
    window.clearInterval(state.pollTimer);
    state.pollTimer = null;
    return;
  }
  try {
    const job = await api(`/api/jobs/${state.activeJobId}`);
    renderJob(job);
    if (job.status !== "queued" && job.status !== "running") {
      window.clearInterval(state.pollTimer);
      state.pollTimer = null;
      setActiveState(`${job.kind}: ${job.status}`, job.status);
      setBusy(false);
      await refreshStatus();
      await refreshReports();
    }
  } catch (error) {
    $("job-log").textContent = String(error);
  }
}

function renderJob(job) {
  $("job-meta").textContent = `${job.id} · ${job.kind} · ${job.status} · rc=${job.returncode ?? "running"}`;
  $("job-log").textContent = [job.command.join(" "), "", job.output || ""].join("\n");
}

function showTab(tab) {
  state.currentTab = tab;
  document.querySelectorAll(".nav-tab").forEach((button) => {
    button.classList.toggle("active", button.dataset.tab === tab);
  });
  document.querySelectorAll(".tab-panel").forEach((panel) => {
    panel.classList.toggle("active", panel.id === `tab-${tab}`);
  });
}

function bindEvents() {
  document.querySelectorAll(".nav-tab").forEach((button) => {
    button.addEventListener("click", () => showTab(button.dataset.tab));
  });
  $("refresh-all").addEventListener("click", async () => {
    await refreshStatus();
    await refreshReports();
    await refreshKnowledgeGraph(false);
  });
  $("refresh-job").addEventListener("click", pollJob);
  $("run-doctor").addEventListener("click", () => startJob("doctor"));
  $("run-discover").addEventListener("click", () =>
    startJob("discover", {
      arch: selectValue("syscall-arch"),
      timeout: numberValue("syscall-timeout"),
      fail_on_diff: checked("syscall-fail-on-diff"),
    }),
  );
  $("run-perf").addEventListener("click", () =>
    startJob("perf-profile", {
      arch: selectValue("perf-arch"),
      timeout: numberValue("perf-timeout"),
      format: selectValue("perf-format"),
      mode: selectValue("perf-mode"),
      freq: numberValue("perf-freq"),
      max_depth: numberValue("perf-depth"),
      top: numberValue("perf-top"),
      min_percent: numberValue("perf-min-percent"),
      host_time: checked("perf-host-time"),
      host_perf: checked("perf-host-perf"),
      host_perf_events: textValue("perf-host-perf-events"),
      shell_init_cmd: textValue("perf-shell-init-cmd"),
      shell_prefix: textValue("perf-shell-prefix"),
      qemu_args: textValue("perf-qemu-args"),
      debug: checked("perf-debug"),
      kernel_filter: checked("perf-kernel-filter"),
    }),
  );
  $("run-diff").addEventListener("click", () =>
    startJob("perf-diff", {
      baseline: selectValue("diff-baseline"),
      compare: selectValue("diff-compare"),
      top: numberValue("diff-top"),
    }),
  );
  $("refresh-kg").addEventListener("click", () => refreshKnowledgeGraph(true));
  $("kg-reset-focus").addEventListener("click", () => {
    state.selectedKnowledgeNode = null;
    if (state.knowledgeGraph) {
      renderKnowledgeGraph(state.knowledgeGraph);
    }
  });
  $("kg-task").addEventListener("change", () => refreshKnowledgeGraph(true));
  $("kg-repo-root").addEventListener("change", () => refreshKnowledgeGraph(true));
  $("kg-hide-empty").addEventListener("change", () => {
    if (state.knowledgeGraph) {
      renderKnowledgeGraph(state.knowledgeGraph);
    }
  });
  $("kg-granularity").addEventListener("change", () => refreshKnowledgeGraph(true));
  $("syscall-arch").addEventListener("change", refreshSyscallReport);
  $("perf-arch").addEventListener("change", refreshPerfReport);
}

async function init() {
  bindEvents();
  await refreshStatus();
  await refreshReports();
  await refreshKnowledgeGraph(false);
}

init().catch((error) => {
  setActiveState("error", "failed");
  $("job-log").textContent = String(error);
});
