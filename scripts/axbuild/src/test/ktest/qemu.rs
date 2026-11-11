use super::*;

pub(super) async fn run_qemu(args: ArgsKtestQemu) -> anyhow::Result<()> {
    let mut app = AppContext::new_with_target_dir(args.target_dir.as_deref())?;
    let packages = discover_workspace_ktests(app.workspace_metadata())?;
    let plan = build_qemu_plan(&packages, &args.qemu_selector())?;
    validate_unique_config_overrides(&args, &plan)?;
    if plan.is_empty() {
        println!("[axtest] no QEMU execution units selected");
        return Ok(());
    }

    println!("[axtest] execution plan ({} unit(s)):", plan.len());
    for unit in &plan {
        println!(
            "  {} --test {} --target {} runtime={}",
            unit.package,
            unit.test,
            unit.target,
            runtime_name(unit.runtime)
        );
    }

    let package_by_name = packages
        .iter()
        .map(|package| (package.name.as_str(), package))
        .collect::<BTreeMap<_, _>>();
    let mut failures = Vec::new();
    for unit in plan {
        let package = package_by_name
            .get(unit.package.as_str())
            .copied()
            .ok_or_else(|| anyhow!("planned package `{}` disappeared", unit.package))?;
        println!(
            "[axtest] running package={} test={} target={}",
            unit.package, unit.test, unit.target
        );
        if let Err(error) = run_qemu_unit(&mut app, &args, package, &unit).await {
            eprintln!(
                "[axtest] failed package={} test={} target={}: {error:#}",
                unit.package, unit.test, unit.target
            );
            failures.push(PlanFailure { unit, error });
            if !args.no_fail_fast {
                break;
            }
        }
    }
    finish_qemu_plan(failures)
}

pub(super) async fn run_qemu_unit(
    app: &mut AppContext,
    args: &ArgsKtestQemu,
    package: &DiscoveredKtestPackage,
    unit: &KtestExecutionUnit,
) -> anyhow::Result<()> {
    let target = package
        .targets
        .iter()
        .find(|target| target.name == unit.test)
        .ok_or_else(|| {
            anyhow!(
                "test target `{}` disappeared from package `{}`",
                unit.test,
                unit.package
            )
        })?;
    let package_dir = package
        .manifest_path
        .parent()
        .ok_or_else(|| anyhow!("invalid manifest path {}", package.manifest_path.display()))?;
    let selected_config = args.config.clone().or_else(|| {
        args.preset
            .is_none()
            .then(|| default_qemu_build_config(package_dir, &unit.target))
            .flatten()
    });
    let qemu_config = args.qemu_config.clone().unwrap_or_else(|| {
        default_qemu_run_config(app.workspace_root(), package_dir, unit.runtime, &unit.arch)
    });
    let qemu_config = app.workspace_root().join(qemu_config).canonicalize()?;
    let parsed_config = KtestBuildConfig::load(
        app.workspace_root(),
        KtestBuildSelection {
            runtime: unit.runtime,
            config: selected_config.as_deref(),
            preset: args.preset.as_deref(),
            default_qemu_target: Some(&unit.target),
        },
    )?;
    let build_config = parsed_config.config_path().map(Path::to_path_buf);
    let mut cargo = parsed_config.cargo(
        app.workspace_context(),
        &KtestUnitConfig {
            package: &unit.package,
            arch: &unit.arch,
            target: &unit.target,
            run_config: &qemu_config,
            config: &parsed_config,
        },
        args.no_default_features,
    )?;
    prepare_ktest_cargo(&mut cargo, target, unit.runtime, args.coverage);
    apply_qemu_cargo_options(&mut cargo, args);
    app.set_debug_mode(false)?;

    // Opt-in resource sidecars next to the run configuration prepare guest
    // sources and shared resources; their outputs must be complete before the
    // kernel build and before anything reads the packed payload.
    crate::axvisor::guest_build::prepare(app, &qemu_config).await?;

    let report_session =
        maybe_start_starry_future_incompat_report(unit.runtime, app.target_dir(), &unit.arch)?;
    let build_result = app.build(cargo.clone(), build_config.clone()).await;
    let output = crate::build::finish_future_incompat_report_session(report_session, build_result)?;
    maybe_postprocess_starry_artifact(
        KtestBuildContext {
            runtime: unit.runtime,
            workspace_root: app.workspace_root(),
            package: &unit.package,
            arch: &unit.arch,
            target: &unit.target,
            config: &parsed_config,
        },
        &cargo,
        &output,
    )?;
    let mut qemu = app
        .read_qemu_config_from_path_for_cargo(&cargo, &qemu_config)
        .await
        .with_context(|| format!("failed to read QEMU config {}", qemu_config.display()))?;
    if unit.runtime == KtestRuntime::Axvisor {
        prepare_axvisor_ktest_payload(
            app,
            &cargo,
            &KtestUnitConfig {
                package: &unit.package,
                arch: &unit.arch,
                target: &unit.target,
                run_config: &qemu_config,
                config: &parsed_config,
            },
            &mut qemu.boot,
            false,
        )
        .await?;
    }
    // A packaged memory root with no host drive is an explicit diskless
    // configuration. It must not acquire an unrelated managed disk/network.
    let rootfs = if unit.runtime == KtestRuntime::Axvisor
        && crate::rootfs::qemu::host_initramfs_without_rootfs_drive(&qemu)
    {
        None
    } else {
        ensure_runtime_qemu_assets(
            unit.runtime,
            app.workspace_root(),
            app.target_dir(),
            &unit.arch,
            &unit.target,
        )
        .await?
    };

    if let Some(rootfs) = rootfs {
        crate::rootfs::qemu::patch_rootfs(
            &mut qemu,
            &rootfs,
            crate::rootfs::qemu::RootfsPatchOptions {
                mode: crate::rootfs::qemu::RootfsPatchMode::EnsureDiskBootNet,
                write_policy: crate::rootfs::qemu::RootfsWritePolicy::Discard,
            },
        )?;
    }
    if unit.runtime == KtestRuntime::Arceos {
        arceos::rootfs::prepare_default_qemu_fat32_rootfs(app.workspace_root(), &qemu)?;
    }
    patch_x86_64_uefi_kernel_loader(&mut qemu, &unit.arch, output.elf_path()).await?;
    apply_ktest_timeout(&mut qemu, unit.runtime, args.coverage);
    apply_axtest_qemu_markers(&mut qemu);
    app.run_qemu_with_axtest_coverage(&cargo, qemu).await?;
    if let Some(out_fmt) = args.out_fmt {
        generate_ktest_coverage_report(out_fmt, app.workspace_root(), &cargo, output.elf_path())?;
    }
    Ok(())
}

pub(super) fn validate_unique_config_overrides(
    args: &ArgsKtestQemu,
    plan: &[KtestExecutionUnit],
) -> anyhow::Result<()> {
    if plan.len() > 1 && (args.config.is_some() || args.qemu_config.is_some()) {
        bail!("--config and --qemu-config require exactly one QEMU execution unit");
    }
    Ok(())
}

pub(super) fn finish_qemu_plan(failures: Vec<PlanFailure>) -> anyhow::Result<()> {
    if failures.is_empty() {
        println!("[axtest] all QEMU execution units passed");
        return Ok(());
    }
    eprintln!("[axtest] {} execution unit(s) failed:", failures.len());
    for failure in &failures {
        eprintln!(
            "  {} --test {} --target {}: {:#}",
            failure.unit.package, failure.unit.test, failure.unit.target, failure.error
        );
    }
    bail!("{} axtest QEMU execution unit(s) failed", failures.len())
}

pub(super) async fn patch_x86_64_uefi_kernel_loader(
    qemu: &mut QemuConfig,
    arch: &str,
    elf_path: &Path,
) -> anyhow::Result<()> {
    if arch != "x86_64" || !qemu.uefi {
        return Ok(());
    }

    let firmware = crate::support::ovmf::OvmfFirmware::fetch(Arch::X64).await?;
    let vars = elf_path.with_extension("vars.fd");
    fs::copy(firmware.vars(), &vars).with_context(|| {
        format!(
            "failed to copy OVMF vars from {} to {}",
            firmware.vars().display(),
            vars.display()
        )
    })?;
    apply_x86_64_uefi_kernel_loader(qemu, firmware.code(), &vars);
    Ok(())
}

pub(super) fn apply_x86_64_uefi_kernel_loader(qemu: &mut QemuConfig, code: &Path, vars: &Path) {
    // Keep ostool's BIN conversion and QEMU kernel loader while supplying
    // the shared OVMF cache as explicit pflash drives.
    qemu.uefi = false;
    qemu.to_bin = true;
    qemu.args.extend([
        "-drive".to_string(),
        format!(
            "if=pflash,format=raw,unit=0,readonly=on,file={}",
            code.display()
        ),
        "-drive".to_string(),
        format!("if=pflash,format=raw,unit=1,file={}", vars.display()),
    ]);
}

pub(super) async fn prepare_axvisor_ktest_payload(
    app: &mut AppContext,
    cargo: &Cargo,
    unit: &KtestUnitConfig<'_>,
    boot: &mut ostool::BootPayloadConfig,
    allow_external_assets: bool,
) -> anyhow::Result<()> {
    let case_dir = unit.run_config.parent().ok_or_else(|| {
        anyhow!(
            "run configuration {} has no parent directory",
            unit.run_config.display()
        )
    })?;
    crate::execution::qemu::prepare_host_initramfs_payload(
        app.workspace_root(),
        app.target_dir(),
        case_dir,
        unit.arch,
        boot,
    )?;
    let mut request = unit.config.axvisor_request(app.workspace_root(), unit)?;
    crate::axvisor::prepare_ktest_guest_payload(
        app,
        &mut request,
        boot,
        cargo,
        case_dir,
        allow_external_assets,
    )
    .await
}

pub(super) async fn ensure_runtime_qemu_assets(
    runtime: KtestRuntime,
    workspace_root: &Path,
    target_dir: &Path,
    arch: &str,
    target: &str,
) -> anyhow::Result<Option<PathBuf>> {
    if runtime == KtestRuntime::Starry {
        let rootfs =
            starry::rootfs::ensure_rootfs_in_tmp_dir(workspace_root, target_dir, arch, target)
                .await?;
        return Ok(Some(rootfs));
    }
    if runtime == KtestRuntime::Axvisor {
        let rootfs =
            crate::image::storage::ensure_rootfs_for_arch(workspace_root, target_dir, arch).await?;
        return Ok(Some(rootfs));
    }
    Ok(None)
}
