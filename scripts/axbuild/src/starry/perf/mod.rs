mod analyzer;
#[path = "args.rs"]
mod args_support;
mod harness;
mod metrics;
mod monitor;
mod outputs;
mod qemu;
mod summary;
mod symbols;
mod toolchain;

use std::path::PathBuf;

use anyhow::bail;

use super::{ArgsBuild, ArgsPerf, PerfFlamegraphKind, PerfFormat, Starry, build, rootfs};
use crate::context::{SnapshotPersistence, StarryCliArgs, starry_target_for_arch_checked};

pub(super) async fn run(starry: &mut Starry, args: ArgsPerf) -> anyhow::Result<()> {
    args_support::validate_args(&args)?;
    let arch = args
        .arch
        .clone()
        .unwrap_or_else(|| crate::context::DEFAULT_STARRY_ARCH.to_string());
    let target = starry_target_for_arch_checked(&arch)?.to_string();
    let outputs = outputs::prepare_outputs(
        starry.app.workspace_root(),
        &arch,
        &args.case,
        args.out.as_deref(),
        args.output_dir.as_deref(),
    )?;
    let _axbuild_tmp_dir = toolchain::set_env_if_missing(
        "AXBUILD_TMP_DIR",
        outputs.work_dir.join("axbuild-tmp").into_os_string(),
    )?;
    let _cross_cc_env = toolchain::prepare_cross_c_compiler_fallback(&outputs.work_dir, &arch)?;
    let generate_svg = args.flamegraph
        || matches!(args.format, PerfFormat::Svg | PerfFormat::All)
            && !matches!(args.flamegraph_kind, PerfFlamegraphKind::Folded);

    let tools = harness::build_qperf_tools(starry.app.workspace_root(), generate_svg)?;

    let build_args = ArgsBuild {
        config: None,
        arch: Some(arch.clone()),
        target: None,
        smp: args.smp,
        debug: args.debug,
    };
    let request = starry.prepare_request(
        StarryCliArgs::from(&build_args),
        None,
        None,
        SnapshotPersistence::Store,
    )?;

    let mut cargo = build::load_cargo_config(&request)?;
    args_support::apply_perf_cargo_features(&mut cargo, &args);
    starry.app.set_debug_mode(args.debug)?;
    let build_output = starry.build_artifact(&request, cargo).await?;
    rootfs::ensure_qemu_rootfs_ready(&request, starry.app.workspace_root(), None).await?;
    let mut cargo = build::load_cargo_config(&request)?;
    args_support::apply_perf_cargo_features(&mut cargo, &args);
    let qemu = rootfs::load_patched_qemu_config(starry, &request, &cargo, None, true).await?;
    let elf = build_output.elf_path().to_path_buf();
    let axconfig_path = cargo.env.get("AX_CONFIG_PATH").map(PathBuf::from);
    let text_range = symbols::detect_kernel_text_range(&elf, axconfig_path.as_deref())?;
    qemu::write_qemu_config(&outputs, &tools, &args, &arch, qemu.args, text_range)?;

    let kernel_bin = symbols::kernel_bin_path(starry.app.workspace_root(), &target, args.debug);
    let qemu_run = qemu::run_qemu_direct(&outputs, &args, &arch, &kernel_bin)?;
    if !qemu_run.status.success() {
        if !outputs::file_nonempty(&outputs.raw) {
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

    analyzer::run_analyzer(analyzer::AnalyzerRun {
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
        min_percent: args_support::flamegraph_min_percent(&args),
    })?;

    analyzer::generate_phase_flamegraphs(
        &tools,
        &elf,
        &outputs,
        &args,
        &qemu_run.window,
        generate_svg,
    )?;
    analyzer::generate_focus_flamegraph(&tools, &elf, &outputs, &args, generate_svg)?;

    let flamegraph_generated = if generate_svg && !outputs::file_nonempty(&outputs.flamegraph) {
        analyzer::try_generate_flamegraph(&outputs.folded, &outputs.flamegraph)?
    } else {
        generate_svg && outputs::file_nonempty(&outputs.flamegraph)
    };

    summary::write_summary(summary::SummaryInputs {
        outputs: &outputs,
        tools: &tools,
        elf: &elf,
        arch: &arch,
        target: &target,
        args: &args,
        flamegraph_generated,
        window: &qemu_run.window,
    })?;
    analyzer::write_flamegraph_html(&outputs, args.flamegraph_kind, flamegraph_generated)?;
    let report_harness = harness::run_report_postprocess(
        starry.app.workspace_root(),
        &outputs,
        &args,
        &arch,
        metrics::exit_status_code(&qemu_run.status),
    )?;
    summary::print_report(&outputs, &args, &report_harness);
    Ok(())
}
