use anyhow::bail;
use ostool::build::config::Cargo;

use super::super::{ArgsPerf, PerfCallchain, PerfFormat};

pub(super) fn apply_perf_cargo_features(cargo: &mut Cargo, args: &ArgsPerf) {
    cargo.features.extend([
        "ax-driver/virtio-blk".to_string(),
        "ax-driver/virtio-net".to_string(),
        "ax-driver/virtio-socket".to_string(),
    ]);
    if args.qperf_metrics {
        cargo.features.push("qperf-metrics".to_string());
    }
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

pub(super) fn validate_args(args: &ArgsPerf) -> anyhow::Result<()> {
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

pub(super) fn host_time_enabled(args: &ArgsPerf) -> bool {
    args.host_time || !args.no_host_time
}

pub(super) fn flamegraph_min_percent(args: &ArgsPerf) -> f64 {
    if args.no_truncate {
        0.0
    } else {
        args.min_percent
    }
}

pub(super) fn effective_max_depth(args: &ArgsPerf) -> usize {
    if args.full_stack {
        args.max_depth.max(256)
    } else {
        args.max_depth
    }
}

pub(super) fn effective_callchain(args: &ArgsPerf) -> PerfCallchain {
    if args.full_stack {
        PerfCallchain::Fp
    } else {
        args.callchain.unwrap_or(PerfCallchain::Leaf)
    }
}

pub(super) fn perf_needs_debuginfo(args: &ArgsPerf) -> bool {
    args.full_stack || args.debuginfo
}

pub(super) fn perf_needs_frame_pointers(args: &ArgsPerf) -> bool {
    args.full_stack
        || args.force_frame_pointers
        || matches!(effective_callchain(args), PerfCallchain::Fp)
}
