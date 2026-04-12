use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use colored::Colorize;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::{
    axvisor::{
        build::{self as axvisor_build, AxvisorBoardFile},
        cases::{
            CasePlan, RunArtifacts,
            build::{PreparedCaseAssets, resolve_runtime_artifact_path},
            manifest::LoadedCase,
            session,
        },
        context::AxvisorContext,
        qemu,
    },
    context::{AppContext, AxvisorCliArgs},
};

const HOST_BOOT_TIMEOUT_SECS: u64 = 30;
const HOST_COMMAND_TIMEOUT_SECS: u64 = 5;
const HOST_VM_CREATE_TIMEOUT_SECS: u64 = 15;
const HOST_GUEST_EXIT_GRACE_SECS: u64 = 2;
const HOST_VM_STOP_TIMEOUT_SECS: u64 = 3;
const HOST_VM_STATE_POLL_INTERVAL_MILLIS: u64 = 200;
const QEMU_ROOTFS_PLACEHOLDER_OLD: &str = "${workspaceFolder}/tmp/rootfs.img";
const QEMU_ROOTFS_PLACEHOLDER_NEW: &str = "${workspaceFolder}/os/axvisor/tmp/rootfs.img";

#[derive(Debug, Clone, Serialize)]
pub(super) struct RunExecution {
    pub(crate) axvisor_build_config: String,
    pub(crate) axvisor_host_log: String,
    pub(crate) cases: Vec<CaseExecutionRecord>,
    pub(crate) passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CaseExecutionRecord {
    pub(crate) id: String,
    pub(crate) asset_key: String,
    pub(crate) raw_log_path: String,
    pub(crate) result_path: Option<String>,
    pub(crate) outcome: CaseOutcome,
    pub(crate) detail: String,
    pub(crate) guest_result: Option<GuestResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum CaseOutcome {
    Passed,
    Failed,
    Skipped,
    Error,
    TimedOut,
}

impl CaseOutcome {
    pub(crate) fn is_success(self) -> bool {
        matches!(self, Self::Passed)
    }

    pub(crate) fn label(self) -> colored::ColoredString {
        match self {
            Self::Passed => "PASS".bold().green(),
            Self::Failed => "FAIL".bold().red(),
            Self::Skipped => "SKIP".bold().yellow(),
            Self::Error => "ERROR".bold().red(),
            Self::TimedOut => "TIMEOUT".bold().red(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct GuestResult {
    pub(crate) case_id: String,
    pub(crate) status: String,
    pub(crate) message: Option<String>,
    pub(crate) details: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
struct PersistedGuestResult<'a> {
    case_id: &'a str,
    status: &'a str,
    message: Option<&'a str>,
    details: Option<&'a Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct GuestResultPayload {
    case_id: String,
    status: String,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    details: Option<Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct QemuConfigFile {
    args: Vec<String>,
    #[serde(default)]
    to_bin: bool,
    #[serde(default)]
    uefi: bool,
}

#[derive(Debug, Clone)]
enum HostSessionAction {
    KeepAlive,
    RestartRequired { reason: String },
}

#[derive(Debug, Clone, Copy)]
enum VmCleanupStrategy {
    GuestShouldSelfExit,
    RunnerMustStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VmLifecycleState {
    Active,
    Stopped,
    Missing,
}

#[derive(Debug, serde::Deserialize)]
struct VmListResponse {
    vms: Vec<VmListEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct VmListEntry {
    id: usize,
    state: String,
}

pub(super) async fn run(
    plan: &CasePlan,
    app: &mut AppContext,
    ctx: &AxvisorContext,
    artifacts: &RunArtifacts,
    prepared_cases: &[PreparedCaseAssets],
) -> anyhow::Result<RunExecution> {
    if plan.cases.len() != prepared_cases.len() {
        bail!(
            "internal error: case/prepared length mismatch (cases={}, prepared={})",
            plan.cases.len(),
            prepared_cases.len()
        );
    }

    let axvisor_build_config =
        write_cases_axvisor_build_config(ctx, &artifacts.run_dir, &plan.arch).with_context(
            || {
                format!(
                    "failed to prepare case-specific AxVisor build config for arch `{}`",
                    plan.arch
                )
            },
        )?;

    let (request, _) = app.prepare_axvisor_request(
        AxvisorCliArgs {
            config: Some(axvisor_build_config.clone()),
            arch: Some(plan.arch.clone()),
            target: None,
            plat_dyn: None,
            smp: None,
            debug: false,
            vmconfigs: vec![],
        },
        None,
        None,
    )?;
    let cargo = axvisor_build::load_cargo_config(&request)?;
    let built = app
        .build_with_artifacts(cargo, request.build_info_path.clone())
        .await
        .context("failed to build AxVisor host for case run")?;
    let runtime = resolve_runtime_artifact_path(&built)
        .context("AxVisor host build finished without runtime artifact")?;
    let runtime = runtime.to_path_buf();
    let qemu_config_path =
        qemu::default_qemu_config_template_path(&request.axvisor_dir, &request.arch);
    let mut session = Some(spawn_target_session(
        &request.arch,
        &runtime,
        &qemu_config_path,
        &artifacts.target_rootfs,
        plan.guest_log,
    )?);

    let mut records = Vec::with_capacity(plan.cases.len());
    let mut host_log = String::new();
    for (index, (case, prepared)) in plan.cases.iter().zip(prepared_cases).enumerate() {
        let (record, action) = {
            let session_ref = session
                .as_mut()
                .expect("target session should exist before running a case");
            run_target_case(case, prepared, session_ref)?
        };

        match action {
            HostSessionAction::KeepAlive => {
                records.push(record);
            }
            HostSessionAction::RestartRequired { reason } => {
                records.push(record);

                let mut current = session
                    .take()
                    .expect("target session should exist when restart is requested");
                append_session_log(
                    &mut host_log,
                    current.buffer(),
                    Some(&format!("host session restart requested: {reason}")),
                );
                current.terminate()?;

                if index + 1 < plan.cases.len() {
                    session = Some(
                        spawn_target_session(
                            &request.arch,
                            &runtime,
                            &qemu_config_path,
                            &artifacts.target_rootfs,
                            plan.guest_log,
                        )
                        .with_context(|| {
                            format!("failed to relaunch AxVisor host after: {reason}")
                        })?,
                    );
                }
            }
        }
    }

    let host_log_path = artifacts.run_dir.join("target-host.raw.log");
    if let Some(mut session) = session {
        append_session_log(&mut host_log, session.buffer(), None);
        session.terminate()?;
    }
    persist_text(&host_log_path, &host_log)?;

    let passed = records.iter().all(|record| record.outcome.is_success());
    Ok(RunExecution {
        axvisor_build_config: axvisor_build_config.display().to_string(),
        axvisor_host_log: host_log_path.display().to_string(),
        cases: records,
        passed,
    })
}

fn run_target_case(
    case: &LoadedCase,
    prepared: &PreparedCaseAssets,
    session: &mut QemuSession,
) -> anyhow::Result<(CaseExecutionRecord, HostSessionAction)> {
    let raw_log_path = prepared.host_case_dir.join("target.raw.log");
    let result_path = prepared.host_case_dir.join("target.result.json");
    let log_start = session.buffer_len();
    let cleanup_vm_id = prepared.vm_id;
    let mut vm_created = false;

    let (guest_result, mut outcome, mut detail) = match create_vm(
        session,
        cleanup_vm_id,
        Path::new(&prepared.guest_vm_config_path),
        &case.manifest.id,
    ) {
        Ok(()) => {
            vm_created = true;
            match start_and_collect_result(case, prepared, session, &result_path) {
                Ok(result) => {
                    let (outcome, detail) = classify_guest_result(&result);
                    (Some(result), outcome, detail)
                }
                Err(err) => {
                    let outcome = classify_runner_error(&err.to_string());
                    (None, outcome, err.to_string())
                }
            }
        }
        Err(err) => {
            let message = err.to_string();
            (None, classify_runner_error(&message), message)
        }
    };

    let (cleanup_message, host_action) = if vm_created {
        let strategy = if guest_result.is_some() {
            VmCleanupStrategy::GuestShouldSelfExit
        } else {
            VmCleanupStrategy::RunnerMustStop
        };
        match cleanup_vm(session, cleanup_vm_id, strategy) {
            Ok(note) => (note, HostSessionAction::KeepAlive),
            Err(err) => (
                Some(err.to_string()),
                HostSessionAction::RestartRequired {
                    reason: format!(
                        "failed to finalize VM[{cleanup_vm_id}] for `{}`",
                        case.manifest.id
                    ),
                },
            ),
        }
    } else {
        (
            None,
            HostSessionAction::RestartRequired {
                reason: format!(
                    "failed to create VM[{cleanup_vm_id}] for `{}`",
                    case.manifest.id
                ),
            },
        )
    };

    let log_end = session.buffer_len();
    let mut raw_log = session.slice(log_start, log_end).to_string();
    if let Some(message) = cleanup_message {
        raw_log.push_str("\n[axcases cleanup warning] ");
        raw_log.push_str(&message);
    }
    if let Some(runtime_failure) = detect_runtime_failure(&raw_log) {
        outcome = CaseOutcome::Error;
        detail = runtime_failure;
    }
    persist_text(&raw_log_path, &raw_log)?;

    Ok((
        CaseExecutionRecord {
            id: case.manifest.id.clone(),
            asset_key: prepared.asset_key.clone(),
            raw_log_path: raw_log_path.display().to_string(),
            result_path: guest_result
                .as_ref()
                .map(|_| result_path.display().to_string()),
            outcome,
            detail,
            guest_result,
        },
        host_action,
    ))
}

fn start_and_collect_result(
    case: &LoadedCase,
    prepared: &PreparedCaseAssets,
    session: &mut QemuSession,
    result_path: &Path,
) -> anyhow::Result<GuestResult> {
    let result_mark = session.buffer_len();
    session.send_line(&session::render_vm_start_cmd(prepared.vm_id))?;
    let _ =
        session.wait_for_prompt_after(result_mark, Duration::from_secs(HOST_COMMAND_TIMEOUT_SECS));
    let result_timeout = Duration::from_secs(case.manifest.timeout_secs + 1);
    let payload = session.wait_for_result_after(result_mark, result_timeout)?;
    let result = parse_guest_result(&payload, &case.manifest.id)?;
    persist_guest_result(result_path, &result)?;
    Ok(result)
}

fn classify_guest_result(result: &GuestResult) -> (CaseOutcome, String) {
    let detail = result
        .message
        .clone()
        .unwrap_or_else(|| format!("guest reported status `{}`", result.status));
    match result.status.as_str() {
        "pass" => (CaseOutcome::Passed, detail),
        "fail" => (CaseOutcome::Failed, detail),
        "skip" => (CaseOutcome::Skipped, detail),
        "error" => (CaseOutcome::Error, detail),
        other => (
            CaseOutcome::Error,
            format!("guest reported unsupported status `{other}`"),
        ),
    }
}

fn classify_runner_error(message: &str) -> CaseOutcome {
    if message.to_ascii_lowercase().contains("timed out") {
        CaseOutcome::TimedOut
    } else {
        CaseOutcome::Error
    }
}

fn cleanup_vm(
    session: &mut QemuSession,
    vm_id: usize,
    strategy: VmCleanupStrategy,
) -> anyhow::Result<Option<String>> {
    let mut note = None;
    match strategy {
        VmCleanupStrategy::GuestShouldSelfExit => {
            match wait_for_vm_terminal_state(
                session,
                vm_id,
                Duration::from_secs(HOST_GUEST_EXIT_GRACE_SECS),
            )? {
                VmLifecycleState::Stopped | VmLifecycleState::Missing => {}
                VmLifecycleState::Active => {
                    stop_vm(session, vm_id)?;
                    note = Some(format!(
                        "guest did not stop itself within {}s; runner issued `vm stop`",
                        HOST_GUEST_EXIT_GRACE_SECS
                    ));
                    wait_for_vm_terminal_state(
                        session,
                        vm_id,
                        Duration::from_secs(HOST_VM_STOP_TIMEOUT_SECS),
                    )?;
                }
            }
        }
        VmCleanupStrategy::RunnerMustStop => {
            stop_vm(session, vm_id)?;
            wait_for_vm_terminal_state(
                session,
                vm_id,
                Duration::from_secs(HOST_VM_STOP_TIMEOUT_SECS),
            )?;
        }
    }

    if query_vm_state(session, vm_id)? != VmLifecycleState::Missing {
        delete_vm(session, vm_id)?;
    }

    Ok(note)
}

fn stop_vm(session: &mut QemuSession, vm_id: usize) -> anyhow::Result<()> {
    let stop_mark = session.buffer_len();
    session.send_line(&session::render_vm_stop_cmd(vm_id))?;
    session
        .wait_for_prompt_after(stop_mark, Duration::from_secs(HOST_COMMAND_TIMEOUT_SECS))
        .context("timed out waiting for `vm stop` prompt")?;
    Ok(())
}

fn delete_vm(session: &mut QemuSession, vm_id: usize) -> anyhow::Result<()> {
    let delete_mark = session.buffer_len();
    session.send_line(&session::render_vm_delete_cmd(vm_id))?;
    session
        .wait_for_prompt_after(delete_mark, Duration::from_secs(HOST_COMMAND_TIMEOUT_SECS))
        .context("timed out waiting for `vm delete` prompt")?;
    wait_for_vm_missing(
        session,
        vm_id,
        Duration::from_secs(HOST_COMMAND_TIMEOUT_SECS),
    )?;
    Ok(())
}

fn create_vm(
    session: &mut QemuSession,
    vm_id: usize,
    config_path: &Path,
    case_id: &str,
) -> anyhow::Result<()> {
    let create_mark = session.buffer_len();
    session.send_line(&session::render_vm_create_cmd(config_path))?;
    let create_output = session
        .wait_for_prompt_after(
            create_mark,
            Duration::from_secs(HOST_VM_CREATE_TIMEOUT_SECS),
        )
        .map_err(|err| {
            err.context(format!(
                "timed out waiting for `vm create` prompt while preparing `{case_id}`\nrecent \
                 host output:\n{}",
                recent_output_tail(&session.buffer()[create_mark..], 24)
            ))
        })?;
    let created_ids = session::parse_created_vm_ids(&create_output);
    if !created_ids.contains(&vm_id) {
        bail!("failed to observe prepared VM id {vm_id} while creating `{case_id}`");
    }
    Ok(())
}

fn wait_for_vm_terminal_state(
    session: &mut QemuSession,
    vm_id: usize,
    timeout: Duration,
) -> anyhow::Result<VmLifecycleState> {
    let deadline = Instant::now() + timeout;
    loop {
        let state = query_vm_state(session, vm_id)?;
        if matches!(state, VmLifecycleState::Stopped | VmLifecycleState::Missing) {
            return Ok(state);
        }
        if Instant::now() >= deadline {
            bail!(
                "VM[{vm_id}] did not reach a terminal state within {}s",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(HOST_VM_STATE_POLL_INTERVAL_MILLIS));
    }
}

fn wait_for_vm_missing(
    session: &mut QemuSession,
    vm_id: usize,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if query_vm_state(session, vm_id)? == VmLifecycleState::Missing {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "VM[{vm_id}] was not removed from the VM list within {}s",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(HOST_VM_STATE_POLL_INTERVAL_MILLIS));
    }
}

fn query_vm_state(session: &mut QemuSession, vm_id: usize) -> anyhow::Result<VmLifecycleState> {
    let mark = session.buffer_len();
    session.send_line(session::render_vm_list_json_cmd())?;
    let output = session
        .wait_for_prompt_after(mark, Duration::from_secs(HOST_COMMAND_TIMEOUT_SECS))
        .context("timed out waiting for `vm list --format json` prompt")?;
    parse_vm_state_from_vm_list_output(&output, vm_id)
}

fn parse_vm_state_from_vm_list_output(
    output: &str,
    vm_id: usize,
) -> anyhow::Result<VmLifecycleState> {
    if output.contains("No virtual machines found.") {
        return Ok(VmLifecycleState::Missing);
    }

    let json = vm_list_json_regex()
        .find_iter(output)
        .last()
        .map(|m| m.as_str())
        .ok_or_else(|| {
            anyhow!("failed to extract JSON payload from `vm list --format json` output")
        })?;
    let parsed: VmListResponse =
        serde_json::from_str(json).context("failed to parse `vm list --format json` output")?;

    let Some(entry) = parsed.vms.into_iter().find(|entry| entry.id == vm_id) else {
        return Ok(VmLifecycleState::Missing);
    };
    let state = entry.state.to_ascii_lowercase();
    if state == "stopped" {
        Ok(VmLifecycleState::Stopped)
    } else {
        Ok(VmLifecycleState::Active)
    }
}

fn vm_list_json_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"(?s)\{\s*"vms"\s*:\s*\[.*?\]\s*\}"#).unwrap())
}

fn spawn_target_session(
    arch: &str,
    runtime: &Path,
    qemu_config_path: &Path,
    rootfs: &Path,
    guest_log: bool,
) -> anyhow::Result<QemuSession> {
    let mut session = QemuSession::spawn(
        arch,
        runtime,
        load_qemu_args(qemu_config_path, Some(rootfs))?,
        guest_log,
    )
    .with_context(|| format!("failed to launch AxVisor host QEMU for arch `{arch}`"))?;

    session
        .wait_for_prompt(Duration::from_secs(HOST_BOOT_TIMEOUT_SECS))
        .context("AxVisor host did not reach shell prompt in time")?;
    Ok(session)
}

fn append_session_log(host_log: &mut String, session_log: &str, header: Option<&str>) {
    if host_log.is_empty() {
        if let Some(header) = header {
            host_log.push_str("[axcases host note] ");
            host_log.push_str(header);
            host_log.push('\n');
        }
        host_log.push_str(session_log);
        return;
    }

    host_log.push_str("\n\n[axcases host session boundary]\n");
    if let Some(header) = header {
        host_log.push_str("[axcases host note] ");
        host_log.push_str(header);
        host_log.push('\n');
    }
    host_log.push_str(session_log);
}

fn recent_output_tail(output: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn parse_guest_result(payload: &str, expected_case_id: &str) -> anyhow::Result<GuestResult> {
    let parsed: GuestResultPayload =
        serde_json::from_str(payload).context("failed to parse guest result payload as JSON")?;
    if parsed.case_id != expected_case_id {
        bail!(
            "guest result case_id mismatch: expected `{expected_case_id}`, got `{}`",
            parsed.case_id
        );
    }
    Ok(GuestResult {
        case_id: parsed.case_id,
        status: parsed.status,
        message: parsed.message,
        details: parsed.details,
    })
}

fn persist_guest_result(path: &Path, result: &GuestResult) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(&PersistedGuestResult {
        case_id: &result.case_id,
        status: &result.status,
        message: result.message.as_deref(),
        details: result.details.as_ref(),
    })?;
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn persist_text(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn detect_runtime_failure(raw_log: &str) -> Option<String> {
    contains_runtime_failure(raw_log)
        .then(|| "runtime failure pattern detected in console log".to_string())
}

fn contains_runtime_failure(raw_log: &str) -> bool {
    runtime_failure_regex().is_match(&normalize_console_for_failure_scan(raw_log))
}

fn runtime_failure_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)panicked?\s+at|kernel panic").unwrap())
}

fn normalize_console_for_failure_scan(raw_log: &str) -> String {
    ansi_escape_regex().replace_all(raw_log, "").into_owned()
}

fn ansi_escape_regex() -> &'static Regex {
    static REGEX: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]").unwrap())
}

fn load_qemu_args(path: &Path, rootfs_override: Option<&Path>) -> anyhow::Result<Vec<String>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let config: QemuConfigFile =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    if config.uefi {
        bail!(
            "case runner does not support UEFI QEMU configs yet: {}",
            path.display()
        );
    }

    let mut args = config.args;
    if let Some(rootfs) = rootfs_override {
        let rootfs = rootfs.display().to_string();
        for arg in &mut args {
            if arg.contains(QEMU_ROOTFS_PLACEHOLDER_OLD) {
                *arg = arg.replace(QEMU_ROOTFS_PLACEHOLDER_OLD, &rootfs);
            }
            if arg.contains(QEMU_ROOTFS_PLACEHOLDER_NEW) {
                *arg = arg.replace(QEMU_ROOTFS_PLACEHOLDER_NEW, &rootfs);
            }
        }
    }
    let _ = config.to_bin;
    Ok(args)
}

fn write_cases_axvisor_build_config(
    ctx: &AxvisorContext,
    run_dir: &Path,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    let board_path = ctx
        .axvisor_dir()
        .join("configs/board")
        .join(format!("qemu-{arch}.toml"));
    let mut board_file: AxvisorBoardFile = axvisor_build::load_board_file(&board_path)?;
    board_file.config.arceos.log = axvisor_build::LogLevel::Warn;
    if !board_file
        .config
        .arceos
        .features
        .iter()
        .any(|feature| feature == "fs")
    {
        board_file.config.arceos.features.push("fs".to_string());
    }
    board_file.config.arceos.features.sort();
    board_file.config.arceos.features.dedup();
    board_file.config.vm_configs.clear();

    let path = run_dir.join(format!("axvisor-cases-{arch}.toml"));
    fs::write(&path, toml::to_string_pretty(&board_file)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn qemu_binary_for_arch(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "aarch64" => Ok("qemu-system-aarch64"),
        "x86_64" => Ok("qemu-system-x86_64"),
        "riscv64" => Ok("qemu-system-riscv64"),
        _ => bail!("unsupported case QEMU arch `{arch}`"),
    }
}

fn render_exit_status(status: ExitStatus) -> String {
    status
        .code()
        .map(|code| format!("exit code {code}"))
        .unwrap_or_else(|| "signal".to_string())
}

struct QemuSession {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    buffer: String,
    echo: bool,
}

impl QemuSession {
    fn spawn(arch: &str, kernel: &Path, args: Vec<String>, echo: bool) -> anyhow::Result<Self> {
        let mut command = Command::new(qemu_binary_for_arch(arch)?);
        command.arg("-kernel").arg(kernel);
        command.args(args);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        let mut child = command.spawn().with_context(|| {
            format!(
                "failed to spawn {} for kernel {}",
                qemu_binary_for_arch(arch).unwrap_or("qemu"),
                kernel.display()
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to take QEMU stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to take QEMU stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("failed to take QEMU stderr"))?;
        let (tx, rx) = mpsc::channel();
        spawn_reader(stdout, tx.clone());
        spawn_reader(stderr, tx);

        Ok(Self {
            child,
            stdin,
            rx,
            buffer: String::new(),
            echo,
        })
    }

    fn send_line(&mut self, line: &str) -> anyhow::Result<()> {
        self.stdin
            .write_all(line.as_bytes())
            .context("failed to write QEMU stdin")?;
        self.stdin
            .write_all(b"\r")
            .context("failed to write QEMU carriage return")?;
        self.stdin.flush().context("failed to flush QEMU stdin")
    }

    fn wait_for_prompt(&mut self, timeout: Duration) -> anyhow::Result<String> {
        self.wait_until(0, timeout, |slice| {
            session::contains_shell_prompt(slice).then(|| slice.to_string())
        })
    }

    fn wait_for_prompt_after(&mut self, start: usize, timeout: Duration) -> anyhow::Result<String> {
        self.wait_until(start, timeout, |slice| {
            session::contains_shell_prompt(slice).then(|| slice.to_string())
        })
    }

    fn wait_for_result_after(&mut self, start: usize, timeout: Duration) -> anyhow::Result<String> {
        self.wait_until(start, timeout, session::extract_result_payload)
    }

    fn wait_until<T, F>(
        &mut self,
        start: usize,
        timeout: Duration,
        predicate: F,
    ) -> anyhow::Result<T>
    where
        F: Fn(&str) -> Option<T>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = predicate(&self.buffer[start..]) {
                return Ok(value);
            }

            if let Some(status) = self.child.try_wait().context("failed to poll QEMU child")? {
                if let Some(value) = predicate(&self.buffer[start..]) {
                    return Ok(value);
                }
                bail!(
                    "QEMU exited before expected output ({})",
                    render_exit_status(status)
                );
            }

            let now = Instant::now();
            if now >= deadline {
                bail!("timed out waiting for expected QEMU output");
            }

            match self.rx.recv_timeout(deadline - now) {
                Ok(chunk) => {
                    if self.echo {
                        print!("{chunk}");
                    }
                    self.buffer.push_str(&chunk);
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(status) =
                        self.child.try_wait().context("failed to poll QEMU child")?
                    {
                        bail!(
                            "QEMU exited while waiting for expected output ({})",
                            render_exit_status(status)
                        );
                    }
                    bail!("timed out waiting for expected QEMU output");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    if let Some(status) =
                        self.child.try_wait().context("failed to poll QEMU child")?
                    {
                        bail!(
                            "QEMU output disconnected after exit ({})",
                            render_exit_status(status)
                        );
                    }
                    bail!("QEMU output disconnected unexpectedly");
                }
            }
        }
    }

    fn buffer(&self) -> &str {
        &self.buffer
    }

    fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    fn slice(&self, start: usize, end: usize) -> &str {
        &self.buffer[start..end]
    }

    fn terminate(&mut self) -> anyhow::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        self.child.kill().context("failed to kill QEMU child")?;
        let _ = self.child.wait();
        Ok(())
    }
}

fn spawn_reader<R>(mut reader: R, tx: mpsc::Sender<String>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = [0_u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = String::from_utf8_lossy(&buf[..read]).into_owned();
                    if tx.send(chunk).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use tempfile::tempdir;

    use super::*;
    use crate::axvisor::cases::{Selection, manifest::CaseManifest};

    #[test]
    fn parse_guest_result_requires_expected_case_id() {
        let err = parse_guest_result(
            r#"{"case_id":"other.case","status":"pass","message":"ok"}"#,
            "timer.basic",
        )
        .unwrap_err();
        assert!(err.to_string().contains("case_id mismatch"));
    }

    #[test]
    fn classify_guest_result_maps_statuses() {
        let result = GuestResult {
            case_id: "timer.basic".to_string(),
            status: "skip".to_string(),
            message: Some("not supported".to_string()),
            details: None,
        };
        let (outcome, detail) = classify_guest_result(&result);
        assert_eq!(outcome, CaseOutcome::Skipped);
        assert_eq!(detail, "not supported");
    }

    #[test]
    fn write_cases_axvisor_build_config_enables_fs_and_clears_vm_configs() {
        let dir = tempdir().unwrap();
        let workspace_root = dir.path().join("workspace");
        let axvisor_dir = workspace_root.join("os/axvisor");
        let board_dir = axvisor_dir.join("configs/board");
        fs::create_dir_all(&board_dir).unwrap();
        fs::write(
            board_dir.join("qemu-aarch64.toml"),
            r#"
env = { AX_IP = "10.0.2.15", AX_GW = "10.0.2.2" }
target = "aarch64-unknown-none-softfloat"
features = ["net"]
log = "Info"
vm_configs = ["vm.toml"]
"#,
        )
        .unwrap();

        fs::create_dir_all(axvisor_dir.join("tmp")).unwrap();
        let ctx = crate::axvisor::context::AxvisorContext::new_in(
            workspace_root.clone(),
            axvisor_dir.clone(),
        );
        let output = write_cases_axvisor_build_config(&ctx, dir.path(), "aarch64").unwrap();
        let body = fs::read_to_string(output).unwrap();
        assert!(body.contains("fs"));
        assert!(!body.contains("filename = \"vm.toml\""));
    }

    #[test]
    fn case_plan_result_summary_round_trip_like_paths() {
        let plan = CasePlan {
            arch: "aarch64".to_string(),
            guest_log: false,
            selection: Selection::Case(PathBuf::from("/tmp/case")),
            suite_name: None,
            cases: vec![crate::axvisor::cases::manifest::LoadedCase {
                case_dir: PathBuf::from("/tmp/case"),
                manifest: CaseManifest {
                    id: "timer.basic".to_string(),
                    tags: vec![],
                    arch: vec!["aarch64".to_string()],
                    timeout_secs: 3,
                    description: None,
                },
            }],
        };
        let artifacts = RunArtifacts {
            run_id: "run-1".to_string(),
            run_dir: PathBuf::from("/tmp/run-1"),
            target_rootfs: PathBuf::from("/tmp/run-1/rootfs.img"),
            summary_path: PathBuf::from("/tmp/run-1/summary.json"),
        };
        let summary = serde_json::to_value(plan.to_summary(&artifacts)).unwrap();
        assert_eq!(summary["arch"], "aarch64");
        assert_eq!(summary["cases"][0]["id"], "timer.basic");
    }
}
