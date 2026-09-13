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
mod test_case;
mod toolchain;

use anyhow::bail;

use super::{ArgsBuild, ArgsPerf, PerfFlamegraphKind, PerfFormat, Starry, build, rootfs};
use crate::context::{SnapshotPersistence, StarryCliArgs, starry_target_for_arch_checked};

pub(super) async fn run(starry: &mut Starry, args: ArgsPerf) -> anyhow::Result<()> {
    args_support::validate_args(&args)?;
    let arch = args
        .arch
        .clone()
        .unwrap_or_else(|| crate::context::DEFAULT_STARRY_ARCH.to_string());
    qemu::validate_arch(&arch)?;
    let target = starry_target_for_arch_checked(&arch)?.to_string();
    let selected_test_case = test_case::resolve(
        starry.app.workspace_root(),
        &arch,
        &target,
        args.test_case.as_deref(),
    )?;
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
        config: selected_test_case
            .as_ref()
            .map(|selected| selected.build_config_path().to_path_buf()),
        arch: Some(arch.clone()),
        target: None,
        smp: args.smp,
        debug: args.debug,
    };
    let request = starry.prepare_request(
        StarryCliArgs::from(&build_args),
        selected_test_case
            .as_ref()
            .map(|selected| selected.qemu_config_path().to_path_buf()),
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
    let mut qemu = rootfs::load_patched_qemu_config(
        starry,
        &request,
        &cargo,
        None,
        true,
        rootfs::RootfsWritePolicy::Discard,
    )
    .await?;
    let prepared_test_case =
        test_case::prepare(starry, &request, selected_test_case.as_ref(), &mut qemu).await?;
    let elf = build_output.elf_path().to_path_buf();
    starry
        .app
        .prepare_elf_artifact(elf.clone(), qemu.to_bin)
        .await?;
    let text_range = symbols::detect_kernel_text_range(&elf)?;
    qemu::write_qemu_config(&outputs, &tools, &args, &arch, &qemu, text_range)?;

    let kernel_bin = symbols::kernel_bin_path(starry.app.workspace_root(), &target, args.debug);
    let qemu_run = qemu::run_qemu_direct(&outputs, &args, &arch, &kernel_bin).await?;
    drop(prepared_test_case);
    let samples_present = outputs::file_nonempty(&outputs.raw);
    validate_qemu_completion(
        qemu_run.status.success(),
        qemu_run.status,
        qemu_run.timed_out,
        samples_present,
    )?;
    if qemu_run.timed_out {
        eprintln!("qperf: completed the configured sampling window after producing samples");
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
        metrics::report_returncode(
            metrics::exit_status_code(&qemu_run.status),
            qemu_run.timed_out,
        ),
    )?;
    summary::print_report(&outputs, &args, &report_harness);
    Ok(())
}

fn validate_qemu_completion(
    status_success: bool,
    status: impl core::fmt::Display,
    timed_out: bool,
    samples_present: bool,
) -> anyhow::Result<()> {
    if !samples_present {
        bail!("qperf QEMU run failed before producing samples: {status}");
    }
    if !status_success && !timed_out {
        bail!("qperf QEMU run failed after producing partial samples: {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_qemu_completion;

    #[test]
    fn partial_samples_do_not_turn_a_failed_qemu_run_into_a_report() {
        let error = validate_qemu_completion(false, "exit status: 1", false, true).unwrap_err();

        assert!(error.to_string().contains("exit status: 1"));
    }

    #[test]
    fn configured_sampling_timeout_keeps_complete_samples() {
        validate_qemu_completion(false, "exit status: 124", true, true).unwrap();
    }

    #[test]
    fn successful_qemu_run_requires_samples() {
        let error = validate_qemu_completion(true, "exit status: 0", false, false).unwrap_err();

        assert!(error.to_string().contains("before producing samples"));
    }

    #[test]
    fn successful_qemu_run_with_samples_is_accepted() {
        validate_qemu_completion(true, "exit status: 0", false, true).unwrap();
    }
}
