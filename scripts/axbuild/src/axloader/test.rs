use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::{Context, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{ArgsTest, ArgsTestQemu, Axloader, TestCommand};
use crate::{
    axvisor::{build, rootfs},
    context::{
        AxvisorCliArgs, ResolvedAxvisorRequest, ResolvedBuildRequest, SnapshotPersistence,
        resolve_axvisor_arch_and_target,
    },
    test::{
        case as test_case, case::TestQemuCase, qemu as test_qemu, qemu::parse_test_target,
        suite as test_suite,
    },
};

const AXLOADER_TEST_SUITE_OS: &str = "axloader";
const AXLOADER_NORMAL_GROUP: &str = "normal";
const ARCEOS_QEMU_GUEST_PACKAGE: &str = "ax-helloworld";
const ARCEOS_QEMU_GUEST_KERNEL_PATH: &str = "/guest/arceos/ax-helloworld-x86_64.bin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxloaderQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedAxloaderQemuCase {
    case: AxloaderQemuCase,
    qemu: QemuConfig,
}

impl test_qemu::BuildConfigRef for PreparedAxloaderQemuCase {
    fn build_group(&self) -> &str {
        &self.case.build_group
    }

    fn build_config_path(&self) -> &Path {
        &self.case.build_config_path
    }
}

pub(super) async fn test(axloader: &mut Axloader, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(args) => axloader.test_qemu(args).await,
    }
}

pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    parse_test_target(
        arch,
        target,
        "axloader qemu tests",
        &crate::context::supported_arches(),
        &crate::context::supported_targets(),
        resolve_axvisor_arch_and_target,
    )
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    group: &str,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<AxloaderQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    test_qemu::discover_qemu_cases(
        &test_suite_dir,
        arch,
        target,
        selected_case,
        "Axloader",
        "qemu",
    )?
    .into_iter()
    .map(load_qemu_case)
    .collect()
}

fn load_qemu_case(case: test_qemu::DiscoveredQemuCase) -> anyhow::Result<AxloaderQemuCase> {
    let build_group = case.build_group;
    let build_config_path = case.build_config_path;
    let test_case = test_qemu::load_test_qemu_case_fields(
        case.display_name,
        case.name,
        case.case_dir,
        case.qemu_config_path,
        "Axloader",
        false,
    )?;
    Ok(AxloaderQemuCase {
        case: test_case,
        build_group,
        build_config_path,
    })
}

fn test_suite_dir(workspace_root: &Path, group: &str) -> anyhow::Result<PathBuf> {
    test_suite::require_group_dir(workspace_root, AXLOADER_TEST_SUITE_OS, "Axloader", group)
}

fn test_suite_root(workspace_root: &Path) -> PathBuf {
    test_suite::suite_root(workspace_root, AXLOADER_TEST_SUITE_OS)
}

fn discover_test_group_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    test_suite::discover_group_names(workspace_root, AXLOADER_TEST_SUITE_OS)
}

fn qemu_list_error_is_ignorable(kind: test_qemu::ListQemuCasesErrorKind) -> bool {
    matches!(
        kind,
        test_qemu::ListQemuCasesErrorKind::EmptyGroup
            | test_qemu::ListQemuCasesErrorKind::UnknownSelectedCase
    )
}

impl Axloader {
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
                        "Axloader",
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
                bail!(
                    "no Axloader qemu test cases found under {}",
                    test_suite_root(self.app.workspace_root()).display()
                );
            }
            println!("{}", test_qemu::render_qemu_case_forest("axloader", groups));
            return Ok(());
        }

        let test_group = args.test_group.as_deref().unwrap_or(AXLOADER_NORMAL_GROUP);
        if args.list && args.arch.is_none() && args.target.is_none() {
            let test_suite_dir = test_suite_dir(self.app.workspace_root(), test_group)?;
            let case_names = test_qemu::discover_all_qemu_cases(
                &test_suite_dir,
                args.test_case.as_deref(),
                "Axloader",
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
            "running axloader qemu tests for arch: {} (target: {}, cases: {})",
            arch,
            target,
            cases.len()
        );

        let request = self.prepare_request(
            axloader_qemu_test_build_args(&arch, None),
            None,
            SnapshotPersistence::Discard,
        )?;
        let request = Self::qemu_test_request(request);
        let cases = self
            .prepare_qemu_cases(&request, cases)
            .await
            .context("failed to load Axloader qemu test cases")?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut summary = test_qemu::QemuTestSummary::default();
        let asset_config = axloader_case_asset_config();

        let build_groups = test_qemu::prepare_case_build_groups(&cases, |build_config_path| {
            Self::qemu_group_build_context(&request, build_config_path)
        })?;

        for build_group in &build_groups {
            rootfs::ensure_qemu_rootfs_ready(&build_group.request, self.app.workspace_root(), None)
                .await?;
            if build_group_needs_arceos_x86_64_guest(&build_group.request) {
                self.build_arceos_x86_64_guest_image()
                    .await
                    .with_context(|| {
                        format!(
                            "failed to build ArceOS guest image for Axloader qemu build group `{}`",
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
                        "failed to build Axloader qemu test artifact for build group `{}` ({})",
                        build_group.group.build_group,
                        build_group.group.build_config_path.display()
                    )
                })?;
        }

        let mut completed = 0;
        for build_group in &build_groups {
            for case in &build_group.group.cases {
                completed += 1;
                let case_name = &case.case.case.name;
                println!("[{completed}/{total}] axloader qemu {case_name}");

                let case_started = Instant::now();
                let result = self
                    .run_qemu_case(
                        &build_group.request,
                        &build_group.cargo,
                        case,
                        &asset_config,
                    )
                    .await
                    .with_context(|| format!("axloader qemu test failed for case `{case_name}`"));
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
        summary.finish_with_total_detail("axloader", "case", Some(total_duration.as_str()))
    }

    async fn prepare_qemu_cases(
        &mut self,
        request: &ResolvedAxvisorRequest,
        cases: Vec<AxloaderQemuCase>,
    ) -> anyhow::Result<Vec<PreparedAxloaderQemuCase>> {
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
                        "failed to read Axloader qemu config for case `{}`",
                        case.case.display_name
                    )
                })?;
            test_qemu::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
            test_qemu::validate_grouped_qemu_commands(&qemu, &case.case, "Axloader")?;
            prepared.push(PreparedAxloaderQemuCase { case, qemu });
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
        let cargo = build::load_cargo_config(&request)?;
        request.vmconfigs = qemu_group_vmconfigs(&request, &cargo)?;

        Ok((request, cargo))
    }

    fn qemu_test_request(mut request: ResolvedAxvisorRequest) -> ResolvedAxvisorRequest {
        request.plat_dyn = None;
        request.smp = None;
        request.vmconfigs.clear();
        request
    }

    async fn load_qemu_case_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
        case: &PreparedAxloaderQemuCase,
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
            self.inject_arceos_x86_64_guest_image(request, case, &mut prepared_assets)
                .with_context(|| {
                    format!(
                        "failed to prepare ArceOS guest image for Axloader qemu case `{}`",
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
        case: &PreparedAxloaderQemuCase,
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

    fn inject_arceos_x86_64_guest_image(
        &self,
        request: &ResolvedAxvisorRequest,
        case: &PreparedAxloaderQemuCase,
        prepared_assets: &mut test_case::PreparedCaseAssets,
    ) -> anyhow::Result<()> {
        let guest_image = arceos_x86_64_guest_bin_path(self.app.workspace_root());
        ensure_file_exists(&guest_image, "ArceOS guest image")?;

        let mut temporary_overlay_run_dir = None;
        let overlay_dir = if prepared_assets.rootfs_copy_to_remove.is_none() {
            let layout = test_case::case_asset_layout(
                self.app.workspace_root(),
                &request.target,
                &case.case.case.display_name,
            )?;
            fs::create_dir_all(&layout.run_dir)
                .with_context(|| format!("failed to create {}", layout.run_dir.display()))?;
            test_case::copy_shared_rootfs_for_case(&prepared_assets.rootfs_path, &layout)?;
            prepared_assets.rootfs_path = layout.case_rootfs_copy.clone();
            prepared_assets.rootfs_copy_to_remove = Some(layout.case_rootfs_copy.clone());
            prepared_assets.run_dir_to_remove = Some(layout.run_dir.clone());
            layout.overlay_dir
        } else {
            let layout = test_case::case_asset_layout(
                self.app.workspace_root(),
                &request.target,
                &case.case.case.display_name,
            )?;
            fs::create_dir_all(&layout.run_dir)
                .with_context(|| format!("failed to create {}", layout.run_dir.display()))?;
            temporary_overlay_run_dir = Some(layout.run_dir);
            layout.overlay_dir
        };
        copy_guest_overlay_file(
            &guest_image,
            &overlay_dir,
            ARCEOS_QEMU_GUEST_KERNEL_PATH,
            "ArceOS guest image",
        )?;
        let result =
            crate::rootfs::inject::inject_overlay(&prepared_assets.rootfs_path, &overlay_dir);
        test_case::remove_case_run_dir(temporary_overlay_run_dir.as_deref());
        result
    }
}

fn arceos_x86_64_guest_request() -> anyhow::Result<ResolvedBuildRequest> {
    let target = "x86_64-unknown-none".to_string();
    Ok(ResolvedBuildRequest {
        package: ARCEOS_QEMU_GUEST_PACKAGE.to_string(),
        arch: "x86_64".to_string(),
        target: target.clone(),
        plat_dyn: Some(false),
        smp: None,
        debug: false,
        build_info_path: crate::arceos::build::resolve_build_info_path(
            ARCEOS_QEMU_GUEST_PACKAGE,
            &target,
            None,
        )?,
        qemu_config: None,
        uboot_config: None,
    })
}

fn arceos_x86_64_guest_elf_path(workspace_root: &Path, debug: bool) -> PathBuf {
    crate::backtrace::arceos_rust_elf_path(
        workspace_root,
        "x86_64-unknown-none",
        ARCEOS_QEMU_GUEST_PACKAGE,
        debug,
    )
}

fn arceos_x86_64_guest_bin_path(workspace_root: &Path) -> PathBuf {
    arceos_x86_64_guest_elf_path(workspace_root, false).with_extension("bin")
}

fn copy_guest_overlay_file(
    source: &Path,
    overlay_dir: &Path,
    guest_path: &str,
    label: &str,
) -> anyhow::Result<()> {
    let overlay_path = overlay_dir.join(guest_path.trim_start_matches('/'));
    if let Some(parent) = overlay_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(source, &overlay_path).with_context(|| {
        format!(
            "failed to copy {label} {} to {}",
            source.display(),
            overlay_path.display()
        )
    })?;
    Ok(())
}

fn build_group_needs_arceos_x86_64_guest(request: &ResolvedAxvisorRequest) -> bool {
    request.arch == "x86_64"
        && request.vmconfigs.iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("arceos"))
        })
}

fn case_needs_arceos_x86_64_guest(
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxloaderQemuCase,
) -> bool {
    build_group_needs_arceos_x86_64_guest(request) || case.case.case.name.contains("arceos")
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

fn axloader_qemu_test_build_args(arch: &str, config: Option<PathBuf>) -> AxvisorCliArgs {
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

fn axloader_case_asset_config() -> test_case::CaseAssetConfig {
    test_case::CaseAssetConfig {
        grouped_runner: test_case::GroupedCaseRunnerConfig {
            runner_name: "axloader-run-case-tests".to_string(),
            runner_path: "/usr/bin/axloader-run-case-tests".to_string(),
            autorun_profile_script: None,
            begin_marker: "AXLOADER_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "AXLOADER_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "AXLOADER_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "AXLOADER_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "AXLOADER_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^AXLOADER_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^AXLOADER_GROUPED_TEST_FAILED:".to_string(),
        },
        script_env: test_case::CaseScriptEnvConfig {
            staging_root: "AXLOADER_TEST_STAGING_ROOT".to_string(),
            case_dir: "AXLOADER_TEST_CASE_DIR".to_string(),
            case_c_dir: "AXLOADER_TEST_CASE_C_DIR".to_string(),
            case_work_dir: "AXLOADER_TEST_CASE_WORK_DIR".to_string(),
            case_build_dir: "AXLOADER_TEST_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "AXLOADER_TEST_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: Vec::new(),
        prepare_staging_root: |_| Ok(()),
        prepare_guest_package_env: None,
    }
}

fn ensure_file_exists(path: &Path, label: &str) -> anyhow::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!("{label} maps to missing file `{}`", path.display())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[derive(serde::Deserialize)]
    struct TestBuildConfigVmConfigs {
        #[serde(default)]
        vm_configs: Vec<PathBuf>,
    }

    fn write_qemu_config(root: &Path, case: &str, arch: &str, body: &str) -> PathBuf {
        write_qemu_config_in_group(root, "normal", "default", case, arch, body)
    }

    fn write_qemu_config_in_group(
        root: &Path,
        group: &str,
        build_group: &str,
        case: &str,
        arch: &str,
        body: &str,
    ) -> PathBuf {
        let dir = root
            .join("test-suit/axloader")
            .join(group)
            .join(build_group)
            .join(case);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("qemu-{arch}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    fn write_qemu_build_config(
        root: &Path,
        group: &str,
        build_group: &str,
        target: &str,
    ) -> PathBuf {
        let dir = root
            .join("test-suit/axloader")
            .join(group)
            .join(build_group);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("build-{target}.toml"));
        fs::write(
            &path,
            format!("target = \"{target}\"\nfeatures = []\nlog = \"Info\"\nvm_configs = []\n"),
        )
        .unwrap();
        path
    }

    fn axloader_request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: PathBuf::from("/tmp/os/axvisor"),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: Vec::new(),
        }
    }

    #[test]
    fn checked_in_test_build_vmconfigs_exist() {
        let workspace_root = std::env::current_dir().unwrap();
        let axloader_suite = workspace_root.join("test-suit/axloader");
        if !axloader_suite.is_dir() {
            return;
        }

        let mut stack = vec![axloader_suite];
        let mut checked = 0;
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }

                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if !file_name.starts_with("build-")
                    || path.extension().and_then(|ext| ext.to_str()) != Some("toml")
                {
                    continue;
                }

                let content = fs::read_to_string(&path).unwrap();
                let config: TestBuildConfigVmConfigs = toml::from_str(&content).unwrap();
                for vm_config in config.vm_configs {
                    if vm_config.starts_with("os/axvisor/tmp/vmconfigs") {
                        continue;
                    }
                    checked += 1;
                    let vm_config_path = if vm_config.is_absolute() {
                        vm_config
                    } else {
                        workspace_root.join(vm_config)
                    };
                    assert!(
                        vm_config_path.is_file(),
                        "{} references missing vm_config {}",
                        path.display(),
                        vm_config_path.display()
                    );
                }
            }
        }

        assert!(checked > 0);
    }

    #[test]
    fn parses_supported_arch_aliases() {
        assert_eq!(
            parse_target(&Some("aarch64".to_string()), &None).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&Some("x86_64".to_string()), &None).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&Some("loongarch64".to_string()), &None).unwrap(),
            (
                "loongarch64".to_string(),
                "loongarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&Some("riscv64".to_string()), &None).unwrap(),
            (
                "riscv64".to_string(),
                "riscv64gc-unknown-none-elf".to_string()
            )
        );
    }

    #[test]
    fn accepts_full_target_triples() {
        assert_eq!(
            parse_target(&None, &Some("aarch64-unknown-none-softfloat".to_string())).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&None, &Some("riscv64gc-unknown-none-elf".to_string())).unwrap(),
            (
                "riscv64".to_string(),
                "riscv64gc-unknown-none-elf".to_string()
            )
        );
        assert_eq!(
            parse_target(
                &None,
                &Some("loongarch64-unknown-none-softfloat".to_string())
            )
            .unwrap(),
            (
                "loongarch64".to_string(),
                "loongarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn rejects_unsupported_arches() {
        let err = parse_target(&Some("mips64".to_string()), &None).unwrap_err();
        let err = err.to_string();

        assert!(err.contains("mips64"));
        assert!(err.contains("aarch64"));
        assert!(err.contains("loongarch64"));
        assert!(err.contains("riscv64"));
        assert!(err.contains("x86_64"));
    }

    #[test]
    fn qemu_test_request_ignores_inherited_smp() {
        let mut request = axloader_request(
            PathBuf::from("/tmp/build-riscv64gc-unknown-none-elf.toml"),
            "riscv64",
            "riscv64gc-unknown-none-elf",
        );
        request.smp = Some(1);

        let request = Axloader::qemu_test_request(request);

        assert_eq!(request.smp, None);
    }

    #[test]
    fn qemu_test_request_ignores_inherited_plat_dyn() {
        let mut request = axloader_request(
            PathBuf::from("/tmp/build-x86_64-unknown-none.toml"),
            "x86_64",
            "x86_64-unknown-none",
        );
        request.plat_dyn = Some(true);

        let request = Axloader::qemu_test_request(request);

        assert_eq!(request.plat_dyn, None);
    }

    #[test]
    fn qemu_test_request_ignores_inherited_vmconfigs() {
        let mut request = axloader_request(
            PathBuf::from("/tmp/build-x86_64-unknown-none.toml"),
            "x86_64",
            "x86_64-unknown-none",
        );
        request
            .vmconfigs
            .push(PathBuf::from("tmp/old-axloader-vm.toml"));

        let request = Axloader::qemu_test_request(request);

        assert!(request.vmconfigs.is_empty());
    }

    #[test]
    fn discovers_only_cases_with_matching_qemu_config() {
        let root = tempdir().unwrap();
        let build_config = write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );
        write_qemu_config(
            root.path(),
            "x86-only",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "normal",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap();

        assert_eq!(
            cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke"]
        );
        assert_eq!(cases[0].build_config_path, build_config);
    }

    #[test]
    fn selected_case_requires_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_qemu_config(
            root.path(),
            "smoke",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );

        let err = discover_qemu_cases(
            root.path(),
            "normal",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            Some("smoke"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("none provide `qemu-aarch64.toml`"));
    }

    #[test]
    fn discovers_qemu_cases_from_selected_group() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(
            root.path(),
            "stress",
            "stress-default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );
        write_qemu_config_in_group(
            root.path(),
            "stress",
            "stress-default",
            "load",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"stress\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        let cases = discover_qemu_cases(
            root.path(),
            "stress",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap();

        assert_eq!(
            cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["load"]
        );
    }

    #[test]
    fn discovers_qemu_cases_from_uefi_group_without_polluting_normal_group() {
        let root = tempdir().unwrap();
        write_qemu_build_config(root.path(), "normal", "default", "x86_64-unknown-none");
        write_qemu_config_in_group(
            root.path(),
            "normal",
            "default",
            "baseline",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );
        write_qemu_build_config(root.path(), "uefi", "qemu-nimbos", "x86_64-unknown-none");
        write_qemu_config_in_group(
            root.path(),
            "uefi",
            "qemu-nimbos",
            "smoke",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );

        let normal_cases =
            discover_qemu_cases(root.path(), "normal", "x86_64", "x86_64-unknown-none", None)
                .unwrap();
        assert_eq!(normal_cases.len(), 1);
        assert_eq!(normal_cases[0].case.name, "baseline");

        let uefi_cases =
            discover_qemu_cases(root.path(), "uefi", "x86_64", "x86_64-unknown-none", None)
                .unwrap();
        assert_eq!(uefi_cases.len(), 1);
        assert_eq!(uefi_cases[0].case.name, "smoke");
        assert_eq!(uefi_cases[0].build_group, "qemu-nimbos");
    }

    #[test]
    fn rejects_unknown_qemu_test_group() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "default",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        let err = discover_qemu_cases(
            root.path(),
            "unknown",
            "aarch64",
            "aarch64-unknown-none-softfloat",
            None,
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported Axloader test group `unknown`")
        );
        assert!(err.to_string().contains("normal"));
    }

    #[test]
    fn x86_linux_direct_boot_configs_keep_timer_calibration_bypass() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for path in [
            "os/axvisor/configs/vms/qemu/x86_64/linux-vmx-smp1.toml",
            "os/axvisor/configs/vms/qemu/x86_64/linux-svm-smp1.toml",
        ] {
            let content = fs::read_to_string(workspace_root.join(path)).unwrap();
            assert!(
                content.contains("no_timer_check"),
                "{path} should keep no_timer_check to avoid x86 Linux guest timer calibration \
                 stalls"
            );
        }
    }
}
