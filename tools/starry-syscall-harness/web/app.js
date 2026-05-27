const state = {
  activeJobId: null,
  pollTimer: null,
  currentTab: "syscall",
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

async function refreshStatus() {
  const status = await api("/api/status");
  $("repo-root").textContent = status.repo_root;
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
    $("perf-functions").innerHTML = emptyRow(3, "no qperf report");
    $("perf-candidates").innerHTML = '<div class="brief-text">no candidates</div>';
    setFlamegraph(null);
  }
}

function renderPerf(report) {
  const hotspots = report.hotspots || {};
  const functions = hotspots.top_functions || [];
  $("perf-report-path").textContent = report._ui.report_path;
  $("perf-summary").innerHTML = [
    summaryItem("Arch", report.arch || "unknown"),
    summaryItem("Result", report.result || "unknown", report.result === "ok" ? "good" : "warn"),
    summaryItem("Samples", hotspots.total_samples || 0),
    summaryItem("Candidates", (report.fix_candidates || []).length),
  ].join("");
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
  setFlamegraph(flamegraph?.exists ? flamegraph.url : null);
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

function setFlamegraph(url) {
  const frame = $("flamegraph-frame");
  const empty = $("flamegraph-empty");
  if (!url) {
    frame.hidden = true;
    frame.removeAttribute("src");
    empty.hidden = false;
    return;
  }
  frame.hidden = false;
  frame.src = url;
  empty.hidden = true;
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
  $("syscall-arch").addEventListener("change", refreshSyscallReport);
  $("perf-arch").addEventListener("change", refreshPerfReport);
}

async function init() {
  bindEvents();
  await refreshStatus();
  await refreshReports();
}

init().catch((error) => {
  setActiveState("error", "failed");
  $("job-log").textContent = String(error);
});
