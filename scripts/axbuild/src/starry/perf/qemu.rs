use std::{
    fs,
    fs::File,
    path::Path,
    process::{Command, ExitStatus},
    time::Instant,
};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use super::{
    super::ArgsPerf,
    args_support::{effective_callchain, effective_max_depth, host_time_enabled},
    harness::QperfTools,
    metrics::{child_resource_usage, write_host_perf_unavailable, write_host_time_metrics},
    monitor::{
        PerfWindowReport, run_qemu_with_stdout_monitor, window_report_from_config,
        write_window_report,
    },
    outputs::{PerfOutputs, ensure_file},
    symbols::KernelTextRange,
    toolchain::find_executable,
};

pub(super) const QPERF_QUEUE_SIZE: usize = 4096;
pub(super) const DEFAULT_STARRY_SHELL_PREFIX: &str = "root@starry:";

#[derive(Deserialize, Serialize)]
pub(super) struct PerfQemuConfig {
    pub(super) args: Vec<String>,
    pub(super) uefi: bool,
    pub(super) to_bin: bool,
    pub(super) success_regex: Vec<String>,
    pub(super) fail_regex: Vec<String>,
    pub(super) shell_prefix: Option<String>,
    pub(super) shell_init_cmd: Option<String>,
    pub(super) timeout: Option<u64>,
    pub(super) start_marker: Option<String>,
    pub(super) stop_marker: Option<String>,
    pub(super) workload_timeout: Option<u64>,
}

pub(super) struct QemuRun {
    pub(super) status: ExitStatus,
    pub(super) window: PerfWindowReport,
}

pub(super) fn write_qemu_config(
    outputs: &PerfOutputs,
    tools: &QperfTools,
    args: &ArgsPerf,
    arch: &str,
    qemu_args: Vec<String>,
    text_range: Option<KernelTextRange>,
) -> anyhow::Result<()> {
    let mut perf_qemu_args = vec!["-plugin".to_string()];
    let mut plugin_params = format!(
        "{},freq={},max_depth={},queue_size={},mode={},callchain={},out={}",
        tools.plugin.display(),
        args.freq,
        effective_max_depth(args),
        QPERF_QUEUE_SIZE,
        args.mode,
        effective_callchain(args),
        outputs.raw.display()
    );
    plugin_params.push_str(&format!(
        ",filter_kernel={}",
        if args.kernel_filter { 1 } else { 0 }
    ));
    if let Some(range) = text_range {
        let start = range.virt.start;
        let end = range.virt.end;
        plugin_params.push_str(&format!(",filter_start=0x{start:x},filter_end=0x{end:x}"));
        if let Some(phys) = range.phys {
            let offset = range.virt.start.wrapping_sub(phys.start);
            plugin_params.push_str(&format!(
                ",filter_alias_start=0x{:x},filter_alias_end=0x{:x},filter_alias_offset=0x{:x}",
                phys.start, phys.end, offset
            ));
        }
    }
    perf_qemu_args.push(plugin_params);
    let mut qemu_args = direct_qemu_args(arch, qemu_args)?;
    qemu_args.extend(args.qemu_args.iter().cloned());
    if qemu_stdout_monitor_enabled(args) && !has_qemu_option(&qemu_args, "-qmp") {
        qemu_args.extend([
            "-qmp".to_string(),
            format!("unix:{},server=on,wait=off", outputs.qmp_socket.display()),
        ]);
    }
    perf_qemu_args.extend(qemu_args);

    let shell_init_cmd = args
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
        .map(str::to_string);
    let shell_prefix = shell_init_cmd.as_ref().map(|_| {
        args.shell_prefix
            .clone()
            .unwrap_or_else(|| DEFAULT_STARRY_SHELL_PREFIX.to_string())
    });

    let config = PerfQemuConfig {
        args: perf_qemu_args,
        uefi: false,
        to_bin: true,
        success_regex: Vec::new(),
        fail_regex: vec![r"(?i)\bpanic(?:ked)?\b".to_string()],
        shell_prefix,
        shell_init_cmd,
        timeout: (args.timeout > 0).then_some(args.timeout),
        start_marker: args.start_marker.clone(),
        stop_marker: args.stop_marker.clone(),
        workload_timeout: args.workload_timeout,
    };
    fs::write(&outputs.qemu_config, toml::to_string_pretty(&config)?)
        .with_context(|| format!("failed to write {}", outputs.qemu_config.display()))?;
    Ok(())
}

fn direct_qemu_args(arch: &str, mut args: Vec<String>) -> anyhow::Result<Vec<String>> {
    match arch {
        "riscv64" | "loongarch64" => {
            if !has_qemu_option(&args, "-machine") {
                args.splice(0..0, ["-machine".to_string(), "virt".to_string()]);
            }
        }
        _ => bail!("qperf currently supports StarryOS riscv64 and loongarch64 only"),
    }
    Ok(args)
}

fn has_qemu_option(args: &[String], option: &str) -> bool {
    args.iter().any(|arg| arg == option)
}

pub(super) fn run_qemu_direct(
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    arch: &str,
    kernel_bin: &Path,
) -> anyhow::Result<QemuRun> {
    ensure_file(kernel_bin, "StarryOS kernel image")?;
    let qemu = qemu_executable(arch)?;
    let config = qemu_config_from_path(&outputs.qemu_config)?;
    let qemu_args = config.args.clone();
    let monitor_stdout = qemu_stdout_monitor_enabled(args);

    let mut command_args = if args.timeout > 0 && !monitor_stdout {
        vec![
            "timeout".to_string(),
            "--signal=INT".to_string(),
            "--kill-after=5s".to_string(),
            format!("{}s", args.timeout),
            qemu.to_string(),
        ]
    } else {
        vec![qemu.to_string()]
    };
    command_args.extend(qemu_args);
    command_args.push("-kernel".to_string());
    command_args.push(kernel_bin.display().to_string());

    if args.host_perf {
        if let Some(perf) = find_executable("perf") {
            let mut wrapped = vec![
                perf.display().to_string(),
                "stat".to_string(),
                "-x".to_string(),
                ",".to_string(),
                "-o".to_string(),
                outputs.host_perf.display().to_string(),
                "-e".to_string(),
                args.host_perf_events.clone(),
                "--".to_string(),
            ];
            wrapped.extend(command_args);
            command_args = wrapped;
        } else {
            write_host_perf_unavailable(&outputs.host_perf, "perf not found in PATH")?;
            eprintln!("qperf: --host-perf requested but `perf` was not found in PATH");
        }
    }

    let mut command = Command::new(&command_args[0]);
    command.args(&command_args[1..]);
    eprintln!("running qperf QEMU: {command:?}");
    let host_wall_start = Instant::now();
    let host_usage_start = child_resource_usage();
    let qemu_run = if monitor_stdout {
        run_qemu_with_stdout_monitor(command, &config, outputs, args.timeout)?
    } else {
        QemuRun {
            status: command.status().context("failed to spawn QEMU")?,
            window: window_report_from_config(&config),
        }
    };
    if host_time_enabled(args) {
        write_host_time_metrics(
            &outputs.host_time,
            host_wall_start.elapsed(),
            host_usage_start,
            child_resource_usage(),
            &qemu_run.status,
        )?;
    }
    write_window_report(&outputs.window, &qemu_run.window)?;
    if !outputs.profile_stdout.exists() {
        File::create(&outputs.profile_stdout)
            .with_context(|| format!("failed to create {}", outputs.profile_stdout.display()))?;
    }
    if !outputs.profile_stderr.exists() {
        File::create(&outputs.profile_stderr)
            .with_context(|| format!("failed to create {}", outputs.profile_stderr.display()))?;
    }
    Ok(qemu_run)
}

fn qemu_executable(arch: &str) -> anyhow::Result<&'static str> {
    let name = match arch {
        "riscv64" => "qemu-system-riscv64",
        "loongarch64" => "qemu-system-loongarch64",
        _ => bail!("qperf currently supports StarryOS riscv64 and loongarch64 only"),
    };
    if find_executable(name).is_none() {
        bail!(
            "qperf requires `{name}` in PATH; install the matching QEMU system emulator or run \
             the Docker-based harness perf-profile entrypoint"
        );
    }
    Ok(name)
}

fn qemu_config_from_path(path: &Path) -> anyhow::Result<PerfQemuConfig> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read qperf QEMU config {}", path.display()))?;
    toml::from_str(&text)
        .with_context(|| format!("failed to parse qperf QEMU config {}", path.display()))
}

fn qemu_stdout_monitor_enabled(args: &ArgsPerf) -> bool {
    args.shell_init_cmd
        .as_deref()
        .is_some_and(|cmd| !cmd.trim().is_empty())
        || args.start_marker.is_some()
        || args.stop_marker.is_some()
        || args.workload_timeout.is_some()
}
