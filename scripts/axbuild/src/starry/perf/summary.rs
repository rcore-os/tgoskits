use std::{fs, fs::File, io::Write, path::Path};

use anyhow::Context;

use super::{
    super::ArgsPerf,
    args_support::{
        effective_callchain, effective_max_depth, flamegraph_min_percent, host_time_enabled,
        perf_needs_debuginfo, perf_needs_frame_pointers,
    },
    harness::QperfTools,
    monitor::PerfWindowReport,
    outputs::{PerfOutputs, count_lines},
    qemu::{DEFAULT_STARRY_SHELL_PREFIX, QPERF_QUEUE_SIZE},
};

pub(super) struct SummaryInputs<'a> {
    pub(super) outputs: &'a PerfOutputs,
    pub(super) tools: &'a QperfTools,
    pub(super) elf: &'a Path,
    pub(super) arch: &'a str,
    pub(super) target: &'a str,
    pub(super) args: &'a ArgsPerf,
    pub(super) flamegraph_generated: bool,
    pub(super) window: &'a PerfWindowReport,
}

pub(super) fn write_summary(input: SummaryInputs<'_>) -> anyhow::Result<()> {
    let outputs = input.outputs;
    let args = input.args;
    let window = input.window;
    let folded_lines = count_lines(&outputs.folded).unwrap_or(0);
    let plugin_summary = outputs.raw.with_extension("summary.txt");
    let plugin_summary_text = fs::read_to_string(plugin_summary).ok();

    let mut file = File::create(&outputs.summary)
        .with_context(|| format!("failed to create {}", outputs.summary.display()))?;
    writeln!(file, "qperf_format_version = 3")?;
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

pub(super) fn print_report(outputs: &PerfOutputs, args: &ArgsPerf, report_harness: &Path) {
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
    println!("  report harness: {}", report_harness.display());
    println!(
        "  compare hint: cargo starry perf-compare is not a cargo subcommand; use python3 {} \
         perf-compare --baseline {} --candidate <other-report.json>",
        report_harness.display(),
        outputs.report_json.display()
    );
}
