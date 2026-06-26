use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{
    AXVISOR_NORMAL_GROUP, AxvisorQemuCase,
    assets::{
        arceos_x86_64_guest_elf_path, arceos_x86_64_guest_request, axvisor_case_asset_config,
        build_group_needs_arceos_x86_64_guest, case_needs_arceos_x86_64_guest,
        inject_arceos_x86_64_guest_image,
    },
    discover_qemu_cases,
    discovery::{
        discover_test_group_names, qemu_list_error_is_ignorable, test_suite_dir, test_suite_root,
    },
    parse_target,
    types::PreparedAxvisorQemuCase,
};
use crate::{
    axvisor::{ArgsTestQemu, Axvisor, build, rootfs},
    build::{append_encoded_rustflags, env_truthy},
    context::{AxvisorCliArgs, ResolvedAxvisorRequest, SnapshotPersistence},
    test::{case as test_case, qemu as test_qemu},
};

const AXTEST_RUSTFLAGS: &[&str] = &["--cfg", "axtest", "--check-cfg", "cfg(axtest)"];

impl Axvisor {
    pub(super) async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        if args.list && args.arch.is_none() && args.target.is_none() && args.test_group.is_none() {
            let groups = discover_test_group_names(self.app.workspace_root())?
                .into_iter()
                .filter_map(|group| {
                    let test_suite_dir = match test_suite_dir(self.app.workspace_root(), &group) {
                        Ok(dir) => dir,
                        Err(err) => return Some(Err(err)),
                    };
                    match test_qemu::discover_all_qemu_cases_with_archs(
                        &test_suite_dir,
                        args.test_case.as_deref(),
                        "Axvisor",
                        &group,
                    ) {
                        Ok(case_names) => Some(Ok((group, case_names))),
                        Err(err) => {
                            if qemu_list_error_is_ignorable(err.kind()) {
                                None
                            } else {
                                Some(Err(anyhow::Error::new(err)))
                            }
                        }
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if groups.is_empty() {
                anyhow::bail!(
                    "no Axvisor qemu test cases found under {}",
                    test_suite_root(self.app.workspace_root()).display()
                );
            }
            println!("{}", test_qemu::render_qemu_case_forest("axvisor", groups));
            return Ok(());
        }

        let test_group = args.test_group.as_deref().unwrap_or(AXVISOR_NORMAL_GROUP);
        if args.list && args.arch.is_none() && args.target.is_none() {
            let test_suite_dir = test_suite_dir(self.app.workspace_root(), test_group)?;
            let case_names = test_qemu::discover_all_qemu_cases(
                &test_suite_dir,
                args.test_case.as_deref(),
                "Axvisor",
                test_group,
            )
            .map_err(anyhow::Error::new)?;
            println!("{}", test_qemu::render_case_tree(test_group, case_names));
            return Ok(());
        }

        let (arch, target) = parse_target(&args.arch, &args.target)?;
        let cases = discover_qemu_cases(
            self.app.workspace_root(),
            test_group,
            &arch,
            &target,
            args.test_case.as_deref(),
        )?;
        if args.list {
            let case_names = cases.iter().map(|case| case.case.name.as_str());
            println!("{}", test_qemu::render_case_tree(test_group, case_names));
            return Ok(());
        }

        println!(
            "running axvisor qemu tests for arch: {} (target: {}, cases: {})",
            arch,
            target,
            cases.len()
        );

        let request = self.prepare_request(
            axvisor_qemu_test_build_args(&arch, None),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let request = Self::qemu_test_request(request);
        let cases = self
            .prepare_qemu_cases(&request, cases)
            .await
            .context("failed to load Axvisor qemu test cases")?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut summary = test_qemu::QemuTestSummary::default();
        let asset_config = axvisor_case_asset_config();

        let mut build_groups = test_qemu::prepare_case_build_groups(&cases, |build_config_path| {
            Self::qemu_group_build_context(&request, build_config_path)
        })?;

        // Phase 1: Build all build groups first so compilation errors surface
        // before any QEMU time is spent.
        for build_group in &mut build_groups {
            rootfs::ensure_qemu_rootfs_ready(&build_group.request, self.app.workspace_root(), None)
                .await?;
            rootfs::prepare_loongarch_linux_vmconfigs(
                &mut build_group.request,
                self.app.workspace_root(),
                None,
            )?;
            build_group.cargo = build::load_cargo_config(&build_group.request)?;
            if build_group_needs_arceos_x86_64_guest(&build_group.request) {
                self.build_arceos_x86_64_guest_image()
                    .await
                    .with_context(|| {
                        format!(
                            "failed to build ArceOS guest image for Axvisor qemu build group `{}`",
                            build_group.group.build_group
                        )
                    })?;
            }
            self.app
                .build(
                    build_group.cargo.clone(),
                    build_group.request.build_info_path.clone(),
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to build Axvisor qemu test artifact for build group `{}` ({})",
                        build_group.group.build_group,
                        build_group.group.build_config_path.display()
                    )
                })?;
        }

        // Phase 2: Run all QEMU tests now that every artifact is available.
        let mut completed = 0;
        for build_group in &build_groups {
            for case in &build_group.group.cases {
                completed += 1;
                let case_name = &case.case.case.name;
                println!("[{completed}/{total}] axvisor qemu {case_name}");

                let case_started = Instant::now();
                let result = self
                    .run_qemu_case(
                        &build_group.request,
                        &build_group.cargo,
                        case,
                        &asset_config,
                    )
                    .await
                    .with_context(|| format!("axvisor qemu test failed for case `{case_name}`"));
                let duration = case_started.elapsed();
                match result {
                    Ok(()) => {
                        println!("ok: {case_name} ({duration:.2?})");
                        summary.pass_with_detail(case_name, format!("{duration:.2?}"));
                    }
                    Err(err) => {
                        eprintln!("failed: {}: {err:#}", case_name);
                        summary.fail_with_detail(case_name, format!("{duration:.2?}"));
                    }
                }
            }
        }

        let total_duration = format!("{:.2?}", suite_started.elapsed());
        summary.finish_with_total_detail("axvisor", "case", Some(total_duration.as_str()))
    }

    async fn prepare_qemu_cases(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cases: Vec<AxvisorQemuCase>,
    ) -> anyhow::Result<Vec<PreparedAxvisorQemuCase>> {
        let mut prepared = Vec::with_capacity(cases.len());
        let mut cargo_by_build_config = BTreeMap::new();
        for case in cases {
            let cargo = Self::qemu_case_cargo_config(
                request,
                &case.build_config_path,
                &mut cargo_by_build_config,
            )?;
            let mut qemu = self
                .app
                .read_qemu_config_from_path_for_cargo(&cargo, &case.case.qemu_config_path)
                .await
                .with_context(|| {
                    format!(
                        "failed to read Axvisor qemu config for case `{}`",
                        case.case.display_name
                    )
                })?;
            test_qemu::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
            test_qemu::validate_grouped_qemu_commands(&qemu, &case.case, "Axvisor")?;
            prepared.push(PreparedAxvisorQemuCase { case, qemu });
        }

        Ok(prepared)
    }

    fn qemu_case_cargo_config(
        request: &ResolvedAxvisorRequest,
        build_config_path: &Path,
        cargo_by_build_config: &mut BTreeMap<PathBuf, Cargo>,
    ) -> anyhow::Result<Cargo> {
        if let Some(cargo) = cargo_by_build_config.get(build_config_path) {
            return Ok(cargo.clone());
        }

        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        let cargo = build::load_cargo_config(&request)?;
        cargo_by_build_config.insert(build_config_path.to_path_buf(), cargo.clone());
        Ok(cargo)
    }

    fn qemu_group_build_context(
        request: &ResolvedAxvisorRequest,
        build_config_path: &Path,
    ) -> anyhow::Result<(ResolvedAxvisorRequest, Cargo)> {
        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        let mut cargo = build::load_cargo_config(&request)?;
        if env_truthy(&cargo.env, "AXTEST") {
            append_encoded_rustflags(&mut cargo, AXTEST_RUSTFLAGS);
            if crate::support::axtest_coverage::enabled(&cargo) {
                crate::support::axtest_coverage::prepare_cargo(&mut cargo);
            }
        }
        request.vmconfigs = qemu_group_vmconfigs(&request, &cargo)?;

        Ok((request, cargo))
    }

    pub(super) fn qemu_test_request(mut request: ResolvedAxvisorRequest) -> ResolvedAxvisorRequest {
        request.plat_dyn = None;
        request.smp = None;
        request.vmconfigs.clear();
        request
    }

    async fn load_qemu_case_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
        case: &PreparedAxvisorQemuCase,
        asset_config: &test_case::CaseAssetConfig,
    ) -> anyhow::Result<(QemuConfig, test_case::PreparedCaseAssets)> {
        let mut qemu = case.qemu.clone();
        test_case::apply_grouped_qemu_config(
            &mut qemu,
            &case.case.case,
            &asset_config.grouped_runner,
        );
        test_qemu::apply_timeout_scale(&mut qemu);

        let rootfs_path = rootfs::qemu_rootfs_path(request, self.app.workspace_root(), None)?;
        let mut prepared_assets = test_case::prepare_case_assets(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
            &case.case.case,
            rootfs_path,
            asset_config.clone(),
        )
        .await?;
        if case_needs_arceos_x86_64_guest(request, case) {
            inject_arceos_x86_64_guest_image(
                self.app.workspace_root(),
                request,
                case,
                &mut prepared_assets,
            )
            .with_context(|| {
                format!(
                    "failed to prepare ArceOS guest image for Axvisor qemu case `{}`",
                    case.case.case.name
                )
            })?;
        }
        rootfs::patch_qemu_rootfs_path(&mut qemu, &prepared_assets.rootfs_path);
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());
        let cargo = build::load_cargo_config(request)?;
        test_qemu::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
        Ok((qemu, prepared_assets))
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cargo: &Cargo,
        case: &PreparedAxvisorQemuCase,
        asset_config: &test_case::CaseAssetConfig,
    ) -> anyhow::Result<()> {
        let prepare_started = Instant::now();
        let (qemu, prepared_assets) = self
            .load_qemu_case_config(request, case, asset_config)
            .await?;
        test_case::run_qemu_with_prepared_case_assets(
            &mut self.app,
            cargo,
            qemu,
            None,
            &case.case.case.qemu_config_path,
            prepared_assets,
            test_case::RunPreparedQemuCaseOptions {
                prepare_elapsed: prepare_started.elapsed(),
                qemu_timing_fields: None,
            },
        )
        .await
    }

    async fn build_arceos_x86_64_guest_image(&mut self) -> anyhow::Result<PathBuf> {
        let request = arceos_x86_64_guest_request()?;
        let cargo = crate::arceos::build::load_cargo_config(&request)?;
        self.app
            .build(cargo.clone(), request.build_info_path.clone())
            .await?;

        let elf_path = arceos_x86_64_guest_elf_path(self.app.workspace_root(), request.debug);
        self.app
            .prepare_elf_artifact(elf_path.clone(), true)
            .await?;

        Ok(elf_path.with_extension("bin"))
    }
}

fn qemu_group_vmconfigs(
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
) -> anyhow::Result<Vec<PathBuf>> {
    let Some(value) = cargo.env.get("AXVISOR_VM_CONFIGS") else {
        return Ok(Vec::new());
    };
    std::env::split_paths(value)
        .map(|path| {
            if path.is_absolute() {
                Ok(path)
            } else {
                Ok(request
                    .axvisor_dir
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(&request.axvisor_dir)
                    .join(path))
            }
        })
        .collect()
}

fn axvisor_qemu_test_build_args(arch: &str, config: Option<PathBuf>) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config,
        arch: Some(arch.to_string()),
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs: Vec::new(),
    }
}
