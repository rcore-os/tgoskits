use std::{
    env,
    ffi::OsString,
    fs,
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail};
use object::{Object, ObjectSection};
use ostool::build::config::Cargo;
use serde::{Deserialize, Serialize};

use super::{
    ArgsBuild, ArgsPerf, PerfCallchain, PerfFlamegraphKind, PerfFormat, Starry, build, rootfs,
};
use crate::{
    context::{SnapshotPersistence, StarryCliArgs, starry_target_for_arch_checked},
    support::process::ProcessExt,
};

const QPERF_QUEUE_SIZE: usize = 4096;
const DEFAULT_STARRY_SHELL_PREFIX: &str = "root@starry:";

#[derive(Deserialize, Serialize)]
struct PerfQemuConfig {
    args: Vec<String>,
    uefi: bool,
    to_bin: bool,
    success_regex: Vec<String>,
    fail_regex: Vec<String>,
    shell_prefix: Option<String>,
    shell_init_cmd: Option<String>,
    timeout: Option<u64>,
    start_marker: Option<String>,
    stop_marker: Option<String>,
    workload_timeout: Option<u64>,
}

struct QperfTools {
    plugin: PathBuf,
    analyzer: PathBuf,
}

struct PerfOutputs {
    work_dir: PathBuf,
    dir: PathBuf,
    raw: PathBuf,
    folded: PathBuf,
    flamegraph: PathBuf,
    folded_boot: PathBuf,
    flamegraph_boot: PathBuf,
    folded_workload: PathBuf,
    flamegraph_workload: PathBuf,
    folded_post: PathBuf,
    flamegraph_post: PathBuf,
    folded_focus: PathBuf,
    flamegraph_focus: PathBuf,
    stack_depth_summary: PathBuf,
    flamegraph_html: PathBuf,
    summary: PathBuf,
    qemu_config: PathBuf,
    host_time: PathBuf,
    host_perf: PathBuf,
    resolve_stats: PathBuf,
    window: PathBuf,
    qmp_socket: PathBuf,
    profile_stdout: PathBuf,
    profile_stderr: PathBuf,
    report_json: PathBuf,
    report_md: PathBuf,
    hotspots_csv: PathBuf,
    hotspot_categories_csv: PathBuf,
}

#[derive(Default, Serialize)]
struct PerfWindowReport {
    enabled: bool,
    start_marker: Option<String>,
    stop_marker: Option<String>,
    start_time: Option<f64>,
    stop_time: Option<f64>,
    duration_sec: Option<f64>,
    workload_timeout: Option<u64>,
    truncated_by_timeout: bool,
    boot_samples_excluded: Option<u64>,
    stop_requested: bool,
    stop_method: Option<String>,
    warnings: Vec<String>,
    method: String,
}

struct QemuRun {
    status: ExitStatus,
    window: PerfWindowReport,
}

#[derive(Clone, Copy, Default)]
struct ChildResourceUsage {
    user_micros: i128,
    system_micros: i128,
    major_faults: i128,
    minor_faults: i128,
    voluntary_context_switches: i128,
    involuntary_context_switches: i128,
}

impl ChildResourceUsage {
    fn delta_since(self, before: Self) -> Self {
        Self {
            user_micros: nonnegative_delta(self.user_micros, before.user_micros),
            system_micros: nonnegative_delta(self.system_micros, before.system_micros),
            major_faults: nonnegative_delta(self.major_faults, before.major_faults),
            minor_faults: nonnegative_delta(self.minor_faults, before.minor_faults),
            voluntary_context_switches: nonnegative_delta(
                self.voluntary_context_switches,
                before.voluntary_context_switches,
            ),
            involuntary_context_switches: nonnegative_delta(
                self.involuntary_context_switches,
                before.involuntary_context_switches,
            ),
        }
    }

    fn user_seconds(self) -> f64 {
        self.user_micros as f64 / 1_000_000.0
    }

    fn system_seconds(self) -> f64 {
        self.system_micros as f64 / 1_000_000.0
    }
}

#[derive(Clone, Copy)]
struct AddressRange {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy)]
struct KernelTextRange {
    virt: AddressRange,
    phys: Option<AddressRange>,
}

pub(super) async fn run(starry: &mut Starry, args: ArgsPerf) -> anyhow::Result<()> {
    validate_args(&args)?;
    let arch = args
        .arch
        .clone()
        .unwrap_or_else(|| crate::context::DEFAULT_STARRY_ARCH.to_string());
    let target = starry_target_for_arch_checked(&arch)?.to_string();
    let outputs = prepare_outputs(
        starry.app.workspace_root(),
        &arch,
        &args.case,
        args.out.as_deref(),
        args.output_dir.as_deref(),
    )?;
    let _axbuild_tmp_dir = set_env_if_missing(
        "AXBUILD_TMP_DIR",
        outputs.work_dir.join("axbuild-tmp").into_os_string(),
    )?;
    let generate_svg = args.flamegraph
        || matches!(args.format, PerfFormat::Svg | PerfFormat::All)
            && !matches!(args.flamegraph_kind, PerfFlamegraphKind::Folded);

    let tools = build_qperf_tools(starry.app.workspace_root(), generate_svg)?;

    let build_args = ArgsBuild {
        config: None,
        arch: Some(arch.clone()),
        target: None,
        smp: None,
        debug: args.debug,
    };
    let request = starry.prepare_request(
        StarryCliArgs::from(&build_args),
        None,
        None,
        SnapshotPersistence::Store,
    )?;

    let mut cargo = build::load_cargo_config(&request)?;
    apply_perf_cargo_features(&mut cargo, &args);
    starry.app.set_debug_mode(args.debug)?;
    starry
        .app
        .build(cargo, request.build_info_path.clone())
        .await?;
    rootfs::ensure_qemu_rootfs_ready(&request, starry.app.workspace_root(), None).await?;
    let mut cargo = build::load_cargo_config(&request)?;
    apply_perf_cargo_features(&mut cargo, &args);
    let qemu = rootfs::load_patched_qemu_config(starry, &request, &cargo, None, true).await?;
    let elf = kernel_elf_path(starry.app.workspace_root(), &target, args.debug);
    let axconfig_path = cargo.env.get("AX_CONFIG_PATH").map(PathBuf::from);
    let text_range = detect_kernel_text_range(&elf, axconfig_path.as_deref())?;
    write_qemu_config(&outputs, &tools, &args, &arch, qemu.args, text_range)?;

    let kernel_bin = kernel_bin_path(starry.app.workspace_root(), &target, args.debug);
    let qemu_run = run_qemu_direct(&outputs, &args, &arch, &kernel_bin)?;
    if !qemu_run.status.success() {
        if !file_nonempty(&outputs.raw) {
            bail!(
                "qperf QEMU run failed before producing samples: {}",
                qemu_run.status
            );
        }
        eprintln!(
            "qperf: QEMU ended with {} after producing samples",
            qemu_run.status
        );
    }

    run_analyzer(AnalyzerRun {
        analyzer: &tools.analyzer,
        elf: &elf,
        raw: &outputs.raw,
        folded: &outputs.folded,
        flamegraph: &outputs.flamegraph,
        resolve_stats: &outputs.resolve_stats,
        depth_summary: Some(&outputs.stack_depth_summary),
        generate_svg,
        top_n: args.top,
        start_sec: qemu_run.window.start_time,
        stop_sec: qemu_run.window.stop_time,
        symbol_style: args.symbol_style.to_string(),
        demangle: true,
        focus: None,
        min_percent: flamegraph_min_percent(&args),
    })?;

    generate_phase_flamegraphs(
        &tools,
        &elf,
        &outputs,
        &args,
        &qemu_run.window,
        generate_svg,
    )?;
    generate_focus_flamegraph(&tools, &elf, &outputs, &args, generate_svg)?;

    let flamegraph_generated = if generate_svg && !file_nonempty(&outputs.flamegraph) {
        try_generate_flamegraph(&outputs.folded, &outputs.flamegraph)?
    } else {
        generate_svg && file_nonempty(&outputs.flamegraph)
    };

    write_summary(SummaryInputs {
        outputs: &outputs,
        tools: &tools,
        elf: &elf,
        arch: &arch,
        target: &target,
        args: &args,
        flamegraph_generated,
        window: &qemu_run.window,
    })?;
    write_flamegraph_html(&outputs, args.flamegraph_kind, flamegraph_generated)?;
    run_report_postprocess(&outputs, &args, &arch, exit_status_code(&qemu_run.status))?;
    print_report(&outputs, &args);
    Ok(())
}

fn apply_perf_cargo_features(cargo: &mut Cargo, args: &ArgsPerf) {
    cargo.features.extend([
        "ax-driver/virtio-blk".to_string(),
        "ax-driver/virtio-net".to_string(),
        "ax-driver/virtio-socket".to_string(),
    ]);
    cargo.features.sort();
    cargo.features.dedup();
    if perf_needs_debuginfo(args) {
        cargo.env.insert("DWARF".to_string(), "y".to_string());
    }
    if perf_needs_frame_pointers(args) {
        cargo.env.insert("BACKTRACE".to_string(), "y".to_string());
    }
    apply_perf_rustflags(cargo, args);
}

fn apply_perf_rustflags(cargo: &mut Cargo, args: &ArgsPerf) {
    let mut flags = Vec::new();
    if perf_needs_debuginfo(args) {
        flags.push("-Cdebuginfo=2".to_string());
        flags.push("-Cstrip=none".to_string());
    }
    if perf_needs_frame_pointers(args) {
        flags.push("-Cforce-frame-pointers=yes".to_string());
    }
    if flags.is_empty() {
        return;
    }

    cargo
        .env
        .insert("CARGO_ENCODED_RUSTFLAGS".to_string(), flags.join("\x1f"));
    cargo.args.push("--config".to_string());
    let rustflags = toml::Value::Array(flags.into_iter().map(toml::Value::String).collect());
    cargo
        .args
        .push(format!("target.'{}'.rustflags={rustflags}", cargo.target));
}

fn validate_args(args: &ArgsPerf) -> anyhow::Result<()> {
    if args.freq == 0 {
        bail!("--freq must be greater than 0");
    }
    if args.max_depth == 0 {
        bail!("--max-depth must be greater than 0");
    }
    if args.min_percent < 0.0 {
        bail!("--min-percent must be non-negative");
    }
    if matches!(args.format, PerfFormat::Pprof) {
        bail!("--format pprof is not supported yet; use --format folded, svg, or all");
    }
    if args
        .shell_init_cmd
        .as_deref()
        .is_some_and(|cmd| cmd.trim().is_empty())
    {
        bail!("--shell-init-cmd must not be empty");
    }
    if args
        .shell_prefix
        .as_deref()
        .is_some_and(|prefix| prefix.is_empty())
    {
        bail!("--shell-prefix must not be empty");
    }
    if args.host_perf && args.host_perf_events.trim().is_empty() {
        bail!("--host-perf-events must not be empty when --host-perf is set");
    }
    if matches!(effective_callchain(args), PerfCallchain::Logical) {
        bail!(
            "--perf-callchain logical is not implemented yet; use --perf-callchain fp or \
             --full-stack for frame-pointer unwinding"
        );
    }
    if args.qperf_metrics {
        eprintln!(
            "qperf: --qperf-metrics parses QPERF_METRIC lines from guest stdout; this tools-only \
             path does not enable kernel-side instrumentation automatically"
        );
    }
    if args.include_user_symbols {
        eprintln!(
            "qperf: --include-user-symbols requested, but current analyzer resolves only the \
             StarryOS kernel ELF; user symbols will remain unresolved unless they are present in \
             the kernel image"
        );
    }
    if args
        .start_marker
        .as_deref()
        .is_some_and(|marker| marker.trim().is_empty())
    {
        bail!("--start-marker must not be empty");
    }
    if args
        .stop_marker
        .as_deref()
        .is_some_and(|marker| marker.trim().is_empty())
    {
        bail!("--stop-marker must not be empty");
    }
    if args.workload_timeout == Some(0) {
        bail!("--workload-timeout must be greater than 0");
    }
    Ok(())
}

fn host_time_enabled(args: &ArgsPerf) -> bool {
    args.host_time || !args.no_host_time
}

fn flamegraph_min_percent(args: &ArgsPerf) -> f64 {
    if args.no_truncate {
        0.0
    } else {
        args.min_percent
    }
}

fn effective_max_depth(args: &ArgsPerf) -> usize {
    if args.full_stack {
        args.max_depth.max(256)
    } else {
        args.max_depth
    }
}

fn effective_callchain(args: &ArgsPerf) -> PerfCallchain {
    if args.full_stack {
        PerfCallchain::Fp
    } else {
        args.callchain.unwrap_or(PerfCallchain::Leaf)
    }
}

fn perf_needs_debuginfo(args: &ArgsPerf) -> bool {
    args.full_stack || args.debuginfo
}

fn perf_needs_frame_pointers(args: &ArgsPerf) -> bool {
    args.full_stack
        || args.force_frame_pointers
        || matches!(effective_callchain(args), PerfCallchain::Fp)
}

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<OsString>,
    active: bool,
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        match &self.previous {
            Some(value) => {
                // SAFETY: qperf runs this CLI flow serially and restores the process
                // environment before returning to the caller.
                unsafe { env::set_var(self.key, value) };
            }
            None => {
                // SAFETY: qperf runs this CLI flow serially and restores the process
                // environment before returning to the caller.
                unsafe { env::remove_var(self.key) };
            }
        }
    }
}

fn set_env_if_missing(key: &'static str, value: OsString) -> anyhow::Result<ScopedEnvVar> {
    let previous = env::var_os(key);
    if previous.as_ref().is_some_and(|value| !value.is_empty()) {
        return Ok(ScopedEnvVar {
            key,
            previous,
            active: false,
        });
    }
    let path = PathBuf::from(&value);
    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create {key} directory {}", path.display()))?;
    // SAFETY: qperf runs this CLI flow serially before spawning worker threads that depend on
    // axbuild paths.
    unsafe { env::set_var(key, &value) };
    Ok(ScopedEnvVar {
        key,
        previous,
        active: true,
    })
}

fn prepare_outputs(
    root: &Path,
    arch: &str,
    case: &str,
    out: Option<&Path>,
    output_dir: Option<&Path>,
) -> anyhow::Result<PerfOutputs> {
    let (work_dir, dir) = if let Some(out) = out {
        let dir = PathBuf::from(out);
        let work_dir = dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.clone());
        (work_dir, dir)
    } else {
        let output_root = output_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target").join("qperf").join(case));
        let work_dir = output_root.join("perf").join(arch).join("latest");
        let dir = work_dir.join("qperf");
        (work_dir, dir)
    };
    if out.is_none() && work_dir.exists() {
        fs::remove_dir_all(&work_dir).with_context(|| {
            format!(
                "failed to remove previous qperf output directory {}",
                work_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create qperf output directory {}", dir.display()))?;
    fs::create_dir_all(&work_dir).with_context(|| {
        format!(
            "failed to create qperf work directory {}",
            work_dir.display()
        )
    })?;
    Ok(PerfOutputs {
        work_dir: work_dir.clone(),
        raw: dir.join("qperf.bin"),
        folded: dir.join("stack.folded"),
        flamegraph: dir.join("flamegraph.svg"),
        folded_boot: dir.join("stack.boot.folded"),
        flamegraph_boot: dir.join("flamegraph.boot.svg"),
        folded_workload: dir.join("stack.workload.folded"),
        flamegraph_workload: dir.join("flamegraph.workload.svg"),
        folded_post: dir.join("stack.post.folded"),
        flamegraph_post: dir.join("flamegraph.post.svg"),
        folded_focus: dir.join("stack.focus.folded"),
        flamegraph_focus: dir.join("flamegraph.focus.svg"),
        stack_depth_summary: dir.join("stack-depth-summary.csv"),
        flamegraph_html: dir.join("flamegraph.html"),
        summary: dir.join("summary.txt"),
        qemu_config: dir.join("qemu.toml"),
        host_time: dir.join("qemu.time.txt"),
        host_perf: dir.join("qemu.perf.csv"),
        resolve_stats: dir.join("resolve.stats.json"),
        window: dir.join("window.json"),
        qmp_socket: dir.join("qmp.sock"),
        profile_stdout: work_dir.join("profile.stdout"),
        profile_stderr: work_dir.join("profile.stderr"),
        report_json: work_dir.join("report.json"),
        report_md: work_dir.join("report.md"),
        hotspots_csv: work_dir.join("hotspots.csv"),
        hotspot_categories_csv: work_dir.join("hotspot_categories.csv"),
        dir,
    })
}

fn build_qperf_tools(root: &Path, analyzer_flamegraph: bool) -> anyhow::Result<QperfTools> {
    let manifest = root.join("tools/qperf/Cargo.toml");
    let target_dir = root.join("tools/qperf/target");
    if !manifest.exists() {
        bail!(
            "qperf sources not found at {}; expected tools/qperf to be present",
            manifest.display()
        );
    }

    Command::new("cargo")
        .current_dir(root)
        .args(["build", "--manifest-path"])
        .arg(&manifest)
        .arg("--release")
        .arg("--target-dir")
        .arg(&target_dir)
        .exec()
        .context("failed to build qperf plugin")?;

    let mut analyzer_build = Command::new("cargo");
    analyzer_build
        .current_dir(root)
        .args(["build", "--manifest-path"])
        .arg(root.join("tools/qperf/analyzer/Cargo.toml"))
        .arg("--release")
        .arg("--target-dir")
        .arg(&target_dir);
    if analyzer_flamegraph {
        analyzer_build.args(["--features", "flamegraph"]);
    }
    analyzer_build
        .exec()
        .context("failed to build qperf-analyzer")?;

    let release_dir = target_dir.join("release");
    let tools = QperfTools {
        plugin: release_dir.join("libqperf.so"),
        analyzer: release_dir.join("qperf-analyzer"),
    };
    ensure_file(&tools.plugin, "qperf plugin")?;
    ensure_file(&tools.analyzer, "qperf analyzer")?;
    Ok(tools)
}

fn write_qemu_config(
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

fn run_qemu_direct(
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

fn window_report_from_config(config: &PerfQemuConfig) -> PerfWindowReport {
    let enabled = config.start_marker.is_some()
        || config.stop_marker.is_some()
        || config.workload_timeout.is_some();
    let mut report = PerfWindowReport {
        enabled,
        start_marker: config.start_marker.clone(),
        stop_marker: config.stop_marker.clone(),
        workload_timeout: config.workload_timeout,
        method: if enabled {
            "qperf_raw_elapsed_timestamp_filter".to_string()
        } else {
            "disabled".to_string()
        },
        ..PerfWindowReport::default()
    };
    if enabled && config.start_marker.is_none() {
        report
            .warnings
            .push("start marker is not configured; boot samples are not excluded".to_string());
    }
    if config.workload_timeout.is_some() && config.start_marker.is_none() {
        report
            .warnings
            .push("--workload-timeout requires a start marker to open the window".to_string());
    }
    report
}

fn run_qemu_with_stdout_monitor(
    mut command: Command,
    config: &PerfQemuConfig,
    outputs: &PerfOutputs,
    overall_timeout: u64,
) -> anyhow::Result<QemuRun> {
    let mut window_report = window_report_from_config(config);
    let shell_init_cmd = config
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty());
    if shell_init_cmd.is_some() {
        command.stdin(Stdio::piped());
    }
    command.stdout(Stdio::piped());
    let mut child = command.spawn().context("failed to spawn QEMU")?;
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().context("failed to open QEMU stdout")?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut buf = [0_u8; 1024];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(len) => {
                    if tx.send(buf[..len].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let started = Instant::now();
    let mut host_stdout = std::io::stdout().lock();
    let mut profile_stdout = File::create(&outputs.profile_stdout)
        .with_context(|| format!("failed to create {}", outputs.profile_stdout.display()))?;
    let mut prompt_window = Vec::new();
    let mut marker_window = Vec::new();
    let mut injected = false;
    let mut echo_disable_deadline = None;
    let shell_prefix = config
        .shell_prefix
        .as_deref()
        .unwrap_or(DEFAULT_STARRY_SHELL_PREFIX);
    let prefix = shell_prefix.as_bytes();
    let start_marker = config.start_marker.as_deref().map(str::as_bytes);
    let stop_marker = config.stop_marker.as_deref().map(str::as_bytes);
    let marker_monitoring = start_marker.is_some() || stop_marker.is_some();

    loop {
        if let Some(status) = child.try_wait().context("failed to poll QEMU")? {
            if shell_init_cmd.is_some() && !injected {
                window_report.warnings.push(format!(
                    "shell prompt `{shell_prefix}` was not observed before QEMU exited"
                ));
                eprintln!(
                    "qperf: shell prompt `{shell_prefix}` was not observed before QEMU exited"
                );
            }
            finalize_window_warnings(&mut window_report);
            return Ok(QemuRun {
                status,
                window: window_report,
            });
        }

        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                profile_stdout
                    .write_all(&chunk)
                    .context("failed to write qperf profile stdout")?;
                host_stdout
                    .write_all(&chunk)
                    .context("failed to forward QEMU stdout")?;
                host_stdout.flush().ok();
                let elapsed = started.elapsed().as_secs_f64();

                if let Some(cmd) = shell_init_cmd
                    && !injected
                    && echo_disable_deadline.is_none()
                {
                    prompt_window.extend_from_slice(&chunk);
                    trim_window(&mut prompt_window, prefix.len().saturating_add(1024));
                    if contains_subslice(&prompt_window, prefix) {
                        let stdin = stdin.as_mut().context("failed to open QEMU stdin")?;
                        if marker_monitoring {
                            stdin
                                .write_all(b"stty -echo 2>/dev/null || true\n")
                                .context("failed to disable shell echo before qperf command")?;
                            stdin.flush().ok();
                            echo_disable_deadline =
                                Some(Instant::now() + Duration::from_millis(150));
                        } else {
                            write_shell_init_command(stdin, cmd)?;
                            injected = true;
                            eprintln!(
                                "qperf: injected shell init command after prompt `{shell_prefix}`"
                            );
                        }
                    }
                }

                if start_marker.is_some() || stop_marker.is_some() {
                    marker_window.extend_from_slice(&chunk);
                    let keep = start_marker
                        .into_iter()
                        .chain(stop_marker)
                        .map(<[u8]>::len)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1024);
                    trim_window(&mut marker_window, keep);
                }

                if window_report.start_time.is_none()
                    && start_marker.is_some_and(|marker| contains_subslice(&marker_window, marker))
                {
                    window_report.start_time = Some(elapsed);
                    eprintln!(
                        "qperf: observed start marker `{}` at {elapsed:.6}s",
                        config.start_marker.as_deref().unwrap_or("")
                    );
                }
                if window_report.stop_time.is_none()
                    && stop_marker.is_some_and(|marker| contains_subslice(&marker_window, marker))
                {
                    window_report.stop_time = Some(elapsed);
                    update_window_duration(&mut window_report);
                    request_qemu_stop(&mut child, outputs, &mut window_report, "stop marker")?;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        if let (Some(cmd), Some(deadline)) = (shell_init_cmd, echo_disable_deadline)
            && !injected
            && Instant::now() >= deadline
        {
            let stdin = stdin.as_mut().context("failed to open QEMU stdin")?;
            write_shell_init_command(stdin, cmd)?;
            injected = true;
            echo_disable_deadline = None;
            eprintln!("qperf: injected shell init command after prompt `{shell_prefix}`");
        }

        let elapsed = started.elapsed().as_secs_f64();
        if let (Some(start_time), Some(timeout)) =
            (window_report.start_time, config.workload_timeout)
            && window_report.stop_time.is_none()
            && elapsed - start_time >= timeout as f64
        {
            window_report.stop_time = Some(elapsed);
            window_report.truncated_by_timeout = true;
            update_window_duration(&mut window_report);
            window_report.warnings.push(format!(
                "workload window timed out after {timeout}s without stop marker"
            ));
            request_qemu_stop(&mut child, outputs, &mut window_report, "workload timeout")?;
            break;
        }
        if overall_timeout > 0 && elapsed >= overall_timeout as f64 {
            window_report.warnings.push(format!(
                "QEMU timed out after {overall_timeout}s before workload completed"
            ));
            request_qemu_stop(&mut child, outputs, &mut window_report, "overall timeout")?;
            break;
        }
    }

    let status = wait_for_child_exit(&mut child, Duration::from_secs(20))?;
    if shell_init_cmd.is_some() && !injected {
        window_report.warnings.push(format!(
            "shell prompt `{shell_prefix}` was not observed before QEMU exited"
        ));
        eprintln!("qperf: shell prompt `{shell_prefix}` was not observed before QEMU exited");
    }
    finalize_window_warnings(&mut window_report);
    Ok(QemuRun {
        status,
        window: window_report,
    })
}

fn trim_window(window: &mut Vec<u8>, keep: usize) {
    if window.len() > keep {
        let drain = window.len() - keep;
        window.drain(..drain);
    }
}

fn write_shell_init_command(stdin: &mut impl Write, cmd: &str) -> anyhow::Result<()> {
    stdin
        .write_all(cmd.as_bytes())
        .context("failed to write qperf shell init command")?;
    stdin
        .write_all(b"\n")
        .context("failed to terminate qperf shell init command")?;
    stdin.flush().ok();
    Ok(())
}

fn update_window_duration(report: &mut PerfWindowReport) {
    report.duration_sec = match (report.start_time, report.stop_time) {
        (Some(start), Some(stop)) if stop >= start => Some(stop - start),
        _ => None,
    };
}

fn request_qemu_stop(
    child: &mut std::process::Child,
    outputs: &PerfOutputs,
    report: &mut PerfWindowReport,
    reason: &str,
) -> anyhow::Result<()> {
    if report.stop_requested {
        return Ok(());
    }
    report.stop_requested = true;
    match request_qmp_quit(&outputs.qmp_socket) {
        Ok(()) => {
            report.stop_method = Some("qmp_quit".to_string());
            eprintln!("qperf: requested QEMU quit via QMP after {reason}");
        }
        Err(err) => {
            report.warnings.push(format!(
                "QMP quit failed after {reason}: {err}; falling back to SIGINT"
            ));
            interrupt_child(child)?;
            report.stop_method = Some("sigint".to_string());
            eprintln!("qperf: sent SIGINT to QEMU after {reason}");
        }
    }
    Ok(())
}

#[cfg(unix)]
fn request_qmp_quit(socket: &Path) -> anyhow::Result<()> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("failed to connect QMP socket {}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    stream
        .set_write_timeout(Some(Duration::from_millis(200)))
        .ok();
    let mut buf = [0_u8; 512];
    let _ = stream.read(&mut buf);
    stream.write_all(b"{\"execute\":\"qmp_capabilities\"}\r\n")?;
    let _ = stream.read(&mut buf);
    stream.write_all(b"{\"execute\":\"quit\"}\r\n")?;
    stream.flush()?;
    Ok(())
}

#[cfg(not(unix))]
fn request_qmp_quit(_socket: &Path) -> anyhow::Result<()> {
    bail!("QMP unix sockets are not supported on this host")
}

#[cfg(unix)]
fn interrupt_child(child: &mut std::process::Child) -> anyhow::Result<()> {
    let pid = child.id() as libc::pid_t;
    if unsafe { libc::kill(pid, libc::SIGINT) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("failed to send SIGINT to QEMU")
    }
}

#[cfg(not(unix))]
fn interrupt_child(child: &mut std::process::Child) -> anyhow::Result<()> {
    child.kill().context("failed to kill QEMU")
}

fn wait_for_child_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> anyhow::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().context("failed to poll QEMU after stop")? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill().context("failed to kill unresponsive QEMU")?;
            return child.wait().context("failed to wait for killed QEMU");
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn finalize_window_warnings(report: &mut PerfWindowReport) {
    if !report.enabled {
        return;
    }
    if report.start_marker.is_some() && report.start_time.is_none() {
        report
            .warnings
            .push("start marker was not observed; folded stacks include boot samples".to_string());
    }
    if report.start_time.is_some() && report.stop_marker.is_some() && report.stop_time.is_none() {
        report
            .warnings
            .push("stop marker was not observed; workload window extends to QEMU exit".to_string());
    }
    update_window_duration(report);
}

fn write_window_report(path: &Path, report: &PerfWindowReport) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(report).context("failed to serialize qperf window")?;
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn write_host_time_metrics(
    path: &Path,
    elapsed: Duration,
    usage_start: Option<ChildResourceUsage>,
    usage_end: Option<ChildResourceUsage>,
    status: &ExitStatus,
) -> anyhow::Result<()> {
    let mut file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    let elapsed_seconds = elapsed.as_secs_f64();
    writeln!(file, "Elapsed time: {elapsed_seconds:.6}")?;
    if let (Some(start), Some(end)) = (usage_start, usage_end) {
        let usage = end.delta_since(start);
        let user_seconds = usage.user_seconds();
        let system_seconds = usage.system_seconds();
        writeln!(file, "User time: {user_seconds:.6}")?;
        writeln!(file, "System time: {system_seconds:.6}")?;
        if elapsed_seconds > 0.0 {
            let cpu_percent = (user_seconds + system_seconds) / elapsed_seconds * 100.0;
            writeln!(file, "Percent of CPU this job got: {cpu_percent:.2}%")?;
        }
        writeln!(file, "Major page faults: {}", usage.major_faults)?;
        writeln!(file, "Minor page faults: {}", usage.minor_faults)?;
        writeln!(
            file,
            "Voluntary context switches: {}",
            usage.voluntary_context_switches
        )?;
        writeln!(
            file,
            "Involuntary context switches: {}",
            usage.involuntary_context_switches
        )?;
    } else {
        writeln!(file, "User time: unavailable")?;
        writeln!(file, "System time: unavailable")?;
    }
    writeln!(file, "Exit status: {}", exit_status_code(status))?;
    Ok(())
}

fn write_host_perf_unavailable(path: &Path, reason: &str) -> anyhow::Result<()> {
    let mut file =
        File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    writeln!(file, "# host perf unavailable: {reason}")?;
    writeln!(
        file,
        "# host perf stat measures the host QEMU process; it is not a guest PMU counter"
    )?;
    Ok(())
}

fn exit_status_code(status: &ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| if status.success() { 0 } else { 1 })
}

fn nonnegative_delta(after: i128, before: i128) -> i128 {
    after.saturating_sub(before).max(0)
}

#[cfg(unix)]
fn child_resource_usage() -> Option<ChildResourceUsage> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the provided rusage pointer when it returns 0.
    if unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: getrusage returned success, so usage is initialized.
    let usage = unsafe { usage.assume_init() };
    Some(ChildResourceUsage {
        user_micros: timeval_micros(usage.ru_utime),
        system_micros: timeval_micros(usage.ru_stime),
        major_faults: usage.ru_majflt.into(),
        minor_faults: usage.ru_minflt.into(),
        voluntary_context_switches: usage.ru_nvcsw.into(),
        involuntary_context_switches: usage.ru_nivcsw.into(),
    })
}

#[cfg(unix)]
fn timeval_micros(value: libc::timeval) -> i128 {
    i128::from(value.tv_sec) * 1_000_000 + i128::from(value.tv_usec)
}

#[cfg(not(unix))]
fn child_resource_usage() -> Option<ChildResourceUsage> {
    None
}

struct AnalyzerRun<'a> {
    analyzer: &'a Path,
    elf: &'a Path,
    raw: &'a Path,
    folded: &'a Path,
    flamegraph: &'a Path,
    resolve_stats: &'a Path,
    depth_summary: Option<&'a Path>,
    generate_svg: bool,
    top_n: usize,
    start_sec: Option<f64>,
    stop_sec: Option<f64>,
    symbol_style: String,
    demangle: bool,
    focus: Option<&'a str>,
    min_percent: f64,
}

fn run_analyzer(args: AnalyzerRun<'_>) -> anyhow::Result<()> {
    ensure_file(args.elf, "StarryOS kernel ELF")?;
    ensure_file(args.raw, "qperf raw samples")?;
    let mut command = Command::new(args.analyzer);
    command
        .arg("resolve")
        .arg("-e")
        .arg(args.elf)
        .arg(args.raw)
        .arg(args.folded);
    if args.top_n > 0 {
        command.arg("--top").arg(args.top_n.to_string());
    }
    if let Some(start_sec) = args.start_sec {
        command.arg("--start-sec").arg(format!("{start_sec:.9}"));
    }
    if let Some(stop_sec) = args.stop_sec {
        command.arg("--stop-sec").arg(format!("{stop_sec:.9}"));
    }
    command
        .arg("--symbol-style")
        .arg(&args.symbol_style)
        .arg("--min-percent")
        .arg(args.min_percent.to_string());
    if !args.demangle {
        command.arg("--no-demangle");
    }
    if let Some(focus) = args.focus {
        command.arg("--focus").arg(focus);
    }
    command.arg("--stats").arg(args.resolve_stats);
    if let Some(depth_summary) = args.depth_summary {
        command.arg("--depth-summary").arg(depth_summary);
    }
    if args.generate_svg {
        command.arg("--flamegraph").arg(args.flamegraph);
    }
    command.exec().context("failed to run qperf-analyzer")?;
    if !args.folded.exists() {
        bail!("folded stack output not found at {}", args.folded.display());
    }
    Ok(())
}

fn generate_phase_flamegraphs(
    tools: &QperfTools,
    elf: &Path,
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    window: &PerfWindowReport,
    generate_svg: bool,
) -> anyhow::Result<()> {
    if window.start_time.is_some() && window.stop_time.is_some() {
        fs::copy(&outputs.folded, &outputs.folded_workload).with_context(|| {
            format!(
                "failed to copy workload folded stack to {}",
                outputs.folded_workload.display()
            )
        })?;
        if file_nonempty(&outputs.flamegraph) {
            fs::copy(&outputs.flamegraph, &outputs.flamegraph_workload).with_context(|| {
                format!(
                    "failed to copy workload flamegraph to {}",
                    outputs.flamegraph_workload.display()
                )
            })?;
        }
    }
    if let Some(start_sec) = window.start_time {
        run_analyzer(AnalyzerRun {
            analyzer: &tools.analyzer,
            elf,
            raw: &outputs.raw,
            folded: &outputs.folded_boot,
            flamegraph: &outputs.flamegraph_boot,
            resolve_stats: &outputs
                .resolve_stats
                .with_file_name("resolve.boot.stats.json"),
            depth_summary: Some(
                &outputs
                    .stack_depth_summary
                    .with_file_name("stack-depth-summary.boot.csv"),
            ),
            generate_svg,
            top_n: 0,
            start_sec: None,
            stop_sec: Some(start_sec),
            symbol_style: args.symbol_style.to_string(),
            demangle: true,
            focus: None,
            min_percent: flamegraph_min_percent(args),
        })?;
    }
    if let Some(stop_sec) = window.stop_time {
        run_analyzer(AnalyzerRun {
            analyzer: &tools.analyzer,
            elf,
            raw: &outputs.raw,
            folded: &outputs.folded_post,
            flamegraph: &outputs.flamegraph_post,
            resolve_stats: &outputs
                .resolve_stats
                .with_file_name("resolve.post.stats.json"),
            depth_summary: Some(
                &outputs
                    .stack_depth_summary
                    .with_file_name("stack-depth-summary.post.csv"),
            ),
            generate_svg,
            top_n: 0,
            start_sec: Some(stop_sec),
            stop_sec: None,
            symbol_style: args.symbol_style.to_string(),
            demangle: true,
            focus: None,
            min_percent: flamegraph_min_percent(args),
        })?;
    }
    Ok(())
}

fn generate_focus_flamegraph(
    tools: &QperfTools,
    elf: &Path,
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    generate_svg: bool,
) -> anyhow::Result<()> {
    let Some(focus) = args.focus.as_deref() else {
        return Ok(());
    };
    run_analyzer(AnalyzerRun {
        analyzer: &tools.analyzer,
        elf,
        raw: &outputs.raw,
        folded: &outputs.folded_focus,
        flamegraph: &outputs.flamegraph_focus,
        resolve_stats: &outputs
            .resolve_stats
            .with_file_name("resolve.focus.stats.json"),
        depth_summary: Some(
            &outputs
                .stack_depth_summary
                .with_file_name("stack-depth-summary.focus.csv"),
        ),
        generate_svg,
        top_n: 0,
        start_sec: None,
        stop_sec: None,
        symbol_style: args.symbol_style.to_string(),
        demangle: true,
        focus: Some(focus),
        min_percent: flamegraph_min_percent(args),
    })
}

fn write_flamegraph_html(
    outputs: &PerfOutputs,
    kind: PerfFlamegraphKind,
    flamegraph_generated: bool,
) -> anyhow::Result<()> {
    if !matches!(kind, PerfFlamegraphKind::Html) || !flamegraph_generated {
        return Ok(());
    }
    let svg = outputs
        .flamegraph
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("flamegraph.svg");
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>StarryOS qperf Flame \
         Graph</title><style>body{{margin:0}}object{{width:100vw;height:100vh;border:0}}</\
         style><object data=\"{svg}\" type=\"image/svg+xml\"></object>\n"
    );
    fs::write(&outputs.flamegraph_html, html)
        .with_context(|| format!("failed to write {}", outputs.flamegraph_html.display()))
}

fn run_report_postprocess(
    outputs: &PerfOutputs,
    args: &ArgsPerf,
    arch: &str,
    returncode: i32,
) -> anyhow::Result<()> {
    let harness = workspace_harness_path(&outputs.work_dir)
        .unwrap_or_else(|| PathBuf::from("tools/starry-syscall-harness/harness.py"));
    if !harness.exists() {
        eprintln!(
            "qperf: harness postprocess script not found at {}; report.json/report.md not \
             generated",
            harness.display()
        );
        return Ok(());
    }
    let mut command = Command::new("python3");
    command
        .arg(&harness)
        .arg("perf-postprocess")
        .arg("--repo-root")
        .arg(workspace_root_from_harness(&harness))
        .arg("--arch")
        .arg(arch)
        .arg("--work-dir")
        .arg(&outputs.work_dir)
        .arg("--qperf-dir")
        .arg(&outputs.dir)
        .arg("--returncode")
        .arg(returncode.to_string())
        .arg("--timeout")
        .arg(args.timeout.to_string())
        .arg("--format")
        .arg(format!("{:?}", args.format).to_ascii_lowercase())
        .arg("--freq")
        .arg(args.freq.to_string())
        .arg("--max-depth")
        .arg(effective_max_depth(args).to_string())
        .arg("--mode")
        .arg(args.mode.to_string())
        .arg("--callchain")
        .arg(effective_callchain(args).to_string())
        .arg("--top")
        .arg(args.top.to_string())
        .arg("--min-percent")
        .arg(args.min_percent.to_string())
        .arg("--symbol-style")
        .arg(args.symbol_style.to_string())
        .arg("--profile-stdout")
        .arg(&outputs.profile_stdout)
        .arg("--profile-stderr")
        .arg(&outputs.profile_stderr);
    if args.debug {
        command.arg("--debug");
    }
    if args.kernel_filter {
        command.arg("--kernel-filter");
    }
    if host_time_enabled(args) {
        command.arg("--host-time");
    }
    if args.host_perf {
        command
            .arg("--host-perf")
            .arg("--host-perf-events")
            .arg(&args.host_perf_events);
    }
    if let Some(cmd) = &args.shell_init_cmd {
        command.arg("--shell-init-cmd").arg(cmd);
    }
    if let Some(prefix) = &args.shell_prefix {
        command.arg("--shell-prefix").arg(prefix);
    }
    if let Some(marker) = &args.start_marker {
        command.arg("--start-marker").arg(marker);
    }
    if let Some(marker) = &args.stop_marker {
        command.arg("--stop-marker").arg(marker);
    }
    if let Some(timeout) = args.workload_timeout {
        command.arg("--workload-timeout").arg(timeout.to_string());
    }
    if args.qperf_metrics {
        command.arg("--qperf-metrics");
    }
    if args.full_stack {
        command.arg("--full-stack");
    }
    if perf_needs_debuginfo(args) {
        command.arg("--perf-debuginfo");
    }
    if perf_needs_frame_pointers(args) {
        command.arg("--perf-force-frame-pointers");
    }
    if let Some(focus) = &args.focus {
        command.arg("--focus").arg(focus);
    }
    if args.no_truncate {
        command.arg("--no-truncate");
    }
    for qemu_arg in &args.qemu_args {
        command.arg("--qemu-arg").arg(qemu_arg);
    }
    let status = command
        .status()
        .context("failed to run qperf report postprocess")?;
    if !status.success() {
        bail!("qperf report postprocess failed with {status}");
    }
    Ok(())
}

fn workspace_harness_path(work_dir: &Path) -> Option<PathBuf> {
    let mut current = Some(work_dir);
    while let Some(path) = current {
        let candidate = path.join("tools/starry-syscall-harness/harness.py");
        if candidate.exists() {
            return Some(candidate);
        }
        current = path.parent();
    }
    None
}

fn workspace_root_from_harness(harness: &Path) -> PathBuf {
    harness
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn try_generate_flamegraph(folded: &Path, svg: &Path) -> anyhow::Result<bool> {
    let Some(generator) = find_flamegraph_generator() else {
        eprintln!(
            "qperf: flamegraph generator not found; install inferno-flamegraph or flamegraph.pl \
             to generate {}",
            svg.display()
        );
        return Ok(false);
    };

    let input =
        File::open(folded).with_context(|| format!("failed to open {}", folded.display()))?;
    let output =
        File::create(svg).with_context(|| format!("failed to create {}", svg.display()))?;
    let status = Command::new(&generator)
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output))
        .status()
        .with_context(|| format!("failed to run {}", generator.display()))?;
    if !status.success() {
        eprintln!(
            "qperf: flamegraph generator {} failed with {status}; folded stacks are still at {}",
            generator.display(),
            folded.display()
        );
        return Ok(false);
    }
    Ok(true)
}

fn find_flamegraph_generator() -> Option<PathBuf> {
    ["inferno-flamegraph", "flamegraph", "flamegraph.pl"]
        .into_iter()
        .find_map(find_executable)
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 && path.is_file() {
        return Some(path.to_path_buf());
    }
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

struct SummaryInputs<'a> {
    outputs: &'a PerfOutputs,
    tools: &'a QperfTools,
    elf: &'a Path,
    arch: &'a str,
    target: &'a str,
    args: &'a ArgsPerf,
    flamegraph_generated: bool,
    window: &'a PerfWindowReport,
}

fn write_summary(input: SummaryInputs<'_>) -> anyhow::Result<()> {
    let outputs = input.outputs;
    let args = input.args;
    let window = input.window;
    let folded_lines = count_lines(&outputs.folded).unwrap_or(0);
    let plugin_summary = outputs.raw.with_extension("summary.txt");
    let plugin_summary_text = fs::read_to_string(plugin_summary).ok();

    let mut file = File::create(&outputs.summary)
        .with_context(|| format!("failed to create {}", outputs.summary.display()))?;
    writeln!(file, "qperf_format_version = 1")?;
    writeln!(file, "arch = {}", input.arch)?;
    writeln!(file, "target = {}", input.target)?;
    writeln!(file, "frequency_hz = {}", args.freq)?;
    writeln!(file, "max_stack_depth = {}", effective_max_depth(args))?;
    writeln!(file, "sampling_mode = {}", args.mode)?;
    writeln!(file, "callchain_mode = {}", effective_callchain(args))?;
    writeln!(file, "full_stack = {}", args.full_stack)?;
    writeln!(file, "perf_debuginfo = {}", perf_needs_debuginfo(args))?;
    writeln!(
        file,
        "perf_force_frame_pointers = {}",
        perf_needs_frame_pointers(args)
    )?;
    writeln!(file, "case = {}", args.case)?;
    writeln!(file, "symbol_style = {}", args.symbol_style)?;
    writeln!(file, "flamegraph_kind = {}", args.flamegraph_kind)?;
    writeln!(
        file,
        "flamegraph_min_percent = {}",
        flamegraph_min_percent(args)
    )?;
    if let Some(focus) = args.focus.as_deref() {
        writeln!(file, "focus = {focus}")?;
    }
    writeln!(file, "kernel_filter = {}", args.kernel_filter)?;
    writeln!(file, "host_time = {}", host_time_enabled(args))?;
    if host_time_enabled(args) {
        writeln!(file, "host_time_output = {}", outputs.host_time.display())?;
    }
    writeln!(file, "host_perf = {}", args.host_perf)?;
    if args.host_perf {
        writeln!(file, "host_perf_events = {}", args.host_perf_events)?;
        writeln!(file, "host_perf_output = {}", outputs.host_perf.display())?;
        writeln!(
            file,
            "host_perf_note = host perf stat measures the QEMU process on the host; it is not a \
             guest PMU cache/cycle counter"
        )?;
    }
    if let Some(shell_init_cmd) = args.shell_init_cmd.as_deref() {
        writeln!(file, "shell_init_cmd = {shell_init_cmd}")?;
        writeln!(
            file,
            "shell_prefix = {}",
            args.shell_prefix
                .as_deref()
                .unwrap_or(DEFAULT_STARRY_SHELL_PREFIX)
        )?;
    }
    if let Some(start_marker) = args.start_marker.as_deref() {
        writeln!(file, "start_marker = {start_marker}")?;
    }
    if let Some(stop_marker) = args.stop_marker.as_deref() {
        writeln!(file, "stop_marker = {stop_marker}")?;
    }
    if let Some(workload_timeout) = args.workload_timeout {
        writeln!(file, "workload_timeout = {workload_timeout}")?;
    }
    writeln!(file, "qperf_metrics = {}", args.qperf_metrics)?;
    writeln!(file, "window_enabled = {}", window.enabled)?;
    if let Some(start_time) = window.start_time {
        writeln!(file, "window_start_time = {start_time:.9}")?;
    }
    if let Some(stop_time) = window.stop_time {
        writeln!(file, "window_stop_time = {stop_time:.9}")?;
    }
    if let Some(duration) = window.duration_sec {
        writeln!(file, "window_duration_sec = {duration:.9}")?;
    }
    writeln!(
        file,
        "window_truncated_by_timeout = {}",
        window.truncated_by_timeout
    )?;
    writeln!(file, "window_report = {}", outputs.window.display())?;
    writeln!(file, "resolve_stats = {}", outputs.resolve_stats.display())?;
    if !args.qemu_args.is_empty() {
        writeln!(file, "extra_qemu_args = {}", args.qemu_args.join(" "))?;
    }
    writeln!(
        file,
        "build_profile = {}",
        if args.debug { "debug" } else { "release" }
    )?;
    writeln!(file, "queue_size = {QPERF_QUEUE_SIZE}")?;
    writeln!(file, "timeout_seconds = {}", args.timeout)?;
    writeln!(file, "kernel_elf = {}", input.elf.display())?;
    writeln!(file, "plugin = {}", input.tools.plugin.display())?;
    writeln!(file, "analyzer = {}", input.tools.analyzer.display())?;
    writeln!(file, "raw_samples = {}", outputs.raw.display())?;
    writeln!(file, "folded_stack = {}", outputs.folded.display())?;
    writeln!(
        file,
        "stack_depth_summary = {}",
        outputs.stack_depth_summary.display()
    )?;
    writeln!(
        file,
        "workload_folded_stack = {}",
        outputs.folded_workload.display()
    )?;
    writeln!(
        file,
        "boot_folded_stack = {}",
        outputs.folded_boot.display()
    )?;
    writeln!(
        file,
        "post_folded_stack = {}",
        outputs.folded_post.display()
    )?;
    writeln!(file, "folded_stack_lines = {folded_lines}")?;
    writeln!(
        file,
        "flamegraph_generated = {}",
        input.flamegraph_generated
    )?;
    if input.flamegraph_generated {
        writeln!(file, "flamegraph = {}", outputs.flamegraph.display())?;
        writeln!(
            file,
            "workload_flamegraph = {}",
            outputs.flamegraph_workload.display()
        )?;
        writeln!(
            file,
            "boot_flamegraph = {}",
            outputs.flamegraph_boot.display()
        )?;
        writeln!(
            file,
            "post_flamegraph = {}",
            outputs.flamegraph_post.display()
        )?;
    }
    if let Some(plugin_summary_text) = plugin_summary_text {
        writeln!(file)?;
        writeln!(file, "[plugin_summary]")?;
        write!(file, "{plugin_summary_text}")?;
    } else {
        writeln!(file, "dropped_samples = unknown")?;
        writeln!(
            file,
            "plugin_summary = unavailable; QEMU may have been stopped by timeout before plugin \
             shutdown"
        )?;
    }
    Ok(())
}

fn print_report(outputs: &PerfOutputs, args: &ArgsPerf) {
    println!("qperf report generated:");
    println!("  report: {}", outputs.report_md.display());
    println!("  flamegraph: {}", outputs.flamegraph.display());
    println!("  folded stack: {}", outputs.folded.display());
    println!(
        "  stack depth summary: {}",
        outputs.stack_depth_summary.display()
    );
    println!("  json: {}", outputs.report_json.display());
    println!("  callchain mode: {}", effective_callchain(args));
    println!("  hotspots: {}", outputs.hotspots_csv.display());
    println!(
        "  hotspot categories: {}",
        outputs.hotspot_categories_csv.display()
    );
    println!("  qperf dir: {}", outputs.dir.display());
    println!("  raw samples: {}", outputs.raw.display());
    if outputs.flamegraph_workload.exists() {
        println!(
            "  workload flamegraph: {}",
            outputs.flamegraph_workload.display()
        );
    }
    if outputs.flamegraph_boot.exists() {
        println!("  boot flamegraph: {}", outputs.flamegraph_boot.display());
    }
    if outputs.flamegraph_post.exists() {
        println!("  post flamegraph: {}", outputs.flamegraph_post.display());
    }
    if outputs.flamegraph_focus.exists() {
        println!(
            "  focused flamegraph: {}",
            outputs.flamegraph_focus.display()
        );
    }
    if outputs.flamegraph_html.exists() {
        println!("  html flamegraph: {}", outputs.flamegraph_html.display());
    }
    println!("  qemu config: {}", outputs.qemu_config.display());
    if outputs.host_time.exists() {
        println!("  host time: {}", outputs.host_time.display());
    }
    if outputs.host_perf.exists() {
        println!("  host perf: {}", outputs.host_perf.display());
    }
    if outputs.window.exists() {
        println!("  window: {}", outputs.window.display());
    }
    println!("  summary: {}", outputs.summary.display());
    println!(
        "  compare hint: cargo starry perf-compare is not a cargo subcommand; use python3 \
         tools/starry-syscall-harness/harness.py perf-compare --baseline {} --candidate \
         <other-report.json>",
        outputs.report_json.display()
    );
}

fn kernel_elf_path(root: &Path, target: &str, debug: bool) -> PathBuf {
    root.join("target")
        .join(target)
        .join(if debug { "debug" } else { "release" })
        .join("starryos")
}

fn kernel_bin_path(root: &Path, target: &str, debug: bool) -> PathBuf {
    root.join("target")
        .join(target)
        .join(if debug { "debug" } else { "release" })
        .join("starryos.bin")
}

fn detect_kernel_text_range(
    elf: &Path,
    axconfig_path: Option<&Path>,
) -> anyhow::Result<Option<KernelTextRange>> {
    if !elf.exists() {
        eprintln!(
            "qperf: kernel ELF not found at {}, skipping .text range filter",
            elf.display()
        );
        return Ok(None);
    }
    let data =
        fs::read(elf).with_context(|| format!("failed to read kernel ELF {}", elf.display()))?;
    let obj = object::File::parse(&*data)
        .map_err(|err| anyhow::anyhow!("failed to parse kernel ELF: {err}"))?;
    let mut virt: Option<AddressRange> = None;
    for section in obj.sections() {
        if !matches!(section.name().unwrap_or(""), ".head.text" | ".text") {
            continue;
        }
        let start = section.address();
        let size = section.size();
        if start == 0 || size == 0 {
            continue;
        }
        let end = start
            .checked_add(size)
            .context("kernel text section end address overflow")?;
        virt = Some(match virt {
            Some(range) => AddressRange {
                start: range.start.min(start),
                end: range.end.max(end),
            },
            None => AddressRange { start, end },
        });
    }

    let Some(virt) = virt else {
        eprintln!(
            "qperf: could not find .head.text/.text sections in kernel ELF, address filter \
             disabled"
        );
        return Ok(None);
    };
    let size = virt.end - virt.start;
    let phys = detect_physical_text_range(virt, size, axconfig_path)?
        .or_else(|| detect_low_address_text_alias(virt));
    eprintln!(
        "qperf: detected kernel text virtual range: 0x{:x}..0x{:x} ({size} bytes)",
        virt.start, virt.end
    );
    if let Some(phys) = phys {
        eprintln!(
            "qperf: detected kernel text physical alias: 0x{:x}..0x{:x}",
            phys.start, phys.end
        );
    }
    Ok(Some(KernelTextRange { virt, phys }))
}

fn detect_physical_text_range(
    virt: AddressRange,
    size: u64,
    axconfig_path: Option<&Path>,
) -> anyhow::Result<Option<AddressRange>> {
    let Some(axconfig_path) = axconfig_path else {
        return Ok(None);
    };
    let Some((kernel_vaddr, kernel_paddr)) = read_kernel_base_addresses(axconfig_path)? else {
        return Ok(None);
    };
    if virt.start < kernel_vaddr {
        return Ok(None);
    }
    let phys_start = kernel_paddr
        .checked_add(virt.start - kernel_vaddr)
        .context("kernel .text physical address overflow")?;
    let phys_end = phys_start
        .checked_add(size)
        .context("kernel .text physical end address overflow")?;
    Ok(Some(AddressRange {
        start: phys_start,
        end: phys_end,
    }))
}

fn detect_low_address_text_alias(virt: AddressRange) -> Option<AddressRange> {
    const LOW_32BIT_MASK: u64 = 0x0000_0000_ffff_ffff;

    if virt.end <= LOW_32BIT_MASK || virt.start & !LOW_32BIT_MASK == 0 {
        return None;
    }
    let size = virt.end.checked_sub(virt.start)?;
    let start = virt.start & LOW_32BIT_MASK;
    let end = start.checked_add(size)?;
    Some(AddressRange { start, end })
}

fn read_kernel_base_addresses(axconfig_path: &Path) -> anyhow::Result<Option<(u64, u64)>> {
    if !axconfig_path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(axconfig_path)
        .with_context(|| format!("failed to read {}", axconfig_path.display()))?;
    let config: toml::Value = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", axconfig_path.display()))?;
    let Some(plat) = config.get("plat").and_then(toml::Value::as_table) else {
        return Ok(None);
    };
    let Some(kernel_vaddr) = plat.get("kernel-base-vaddr").and_then(parse_axconfig_uint) else {
        return Ok(None);
    };
    let Some(kernel_paddr) = plat.get("kernel-base-paddr").and_then(parse_axconfig_uint) else {
        return Ok(None);
    };
    Ok(Some((kernel_vaddr, kernel_paddr)))
}

fn parse_axconfig_uint(value: &toml::Value) -> Option<u64> {
    match value {
        toml::Value::Integer(value) => (*value).try_into().ok(),
        toml::Value::String(value) => parse_u64_literal(value),
        _ => None,
    }
}

fn parse_u64_literal(value: &str) -> Option<u64> {
    let compact = value.trim().replace('_', "");
    if let Some(hex) = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        compact.parse().ok()
    }
}

fn ensure_file(path: &Path, label: &str) -> anyhow::Result<()> {
    if file_nonempty(path) {
        Ok(())
    } else {
        bail!("{label} not found or empty at {}", path.display())
    }
}

fn file_nonempty(path: &Path) -> bool {
    path.metadata().map(|meta| meta.len() > 0).unwrap_or(false)
}

fn count_lines(path: &Path) -> anyhow::Result<u64> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut count = 0;
    for line in BufReader::new(file).lines() {
        line?;
        count += 1;
    }
    Ok(count)
}
