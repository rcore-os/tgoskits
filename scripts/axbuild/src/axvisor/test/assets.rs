use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

use super::{discovery::ensure_file_exists, types::PreparedAxvisorQemuCase};
use crate::{
    context::{ResolvedAxvisorRequest, ResolvedBuildRequest},
    test::case as test_case,
};

const ARCEOS_QEMU_GUEST_PACKAGE: &str = "ax-helloworld";
const ARCEOS_QEMU_GUEST_KERNEL_PATH: &str = "/guest/arceos/ax-helloworld-x86_64.bin";
const ARCEOS_IVC_AARCH64_GUEST_PACKAGES: &[&str] =
    &["arceos-ivc-publisher", "arceos-ivc-subscriber"];

#[derive(Clone, Copy)]
struct ArceosIvcGuestProfile {
    arch: &'static str,
    target: &'static str,
    packages: &'static [&'static str],
    vmconfig_marker: &'static str,
}

const ARCEOS_IVC_GUEST_PROFILES: &[ArceosIvcGuestProfile] = &[ArceosIvcGuestProfile {
    arch: "aarch64",
    target: "aarch64-unknown-none-softfloat",
    packages: ARCEOS_IVC_AARCH64_GUEST_PACKAGES,
    vmconfig_marker: "ivc",
}];

pub(super) fn arceos_x86_64_guest_request() -> anyhow::Result<ResolvedBuildRequest> {
    arceos_guest_request(ARCEOS_QEMU_GUEST_PACKAGE, "x86_64", "x86_64-unknown-none")
}

pub(super) fn arceos_ivc_guest_requests(
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<Vec<ResolvedBuildRequest>> {
    matching_arceos_ivc_guest_profiles(request)
        .flat_map(|profile| {
            profile
                .packages
                .iter()
                .map(move |package| arceos_guest_request(package, profile.arch, profile.target))
        })
        .collect()
}

fn arceos_guest_request(
    package: &str,
    arch: &str,
    target: &str,
) -> anyhow::Result<ResolvedBuildRequest> {
    let target = target.to_string();
    Ok(ResolvedBuildRequest {
        package: package.to_string(),
        arch: arch.to_string(),
        target: target.clone(),
        smp: None,
        debug: false,
        build_info_path: crate::arceos::build::resolve_build_info_path(package, &target, None)?,
        qemu_config: None,
        uboot_config: None,
    })
}

pub(super) fn arceos_x86_64_guest_elf_path(workspace_root: &Path, debug: bool) -> PathBuf {
    arceos_guest_elf_path(
        workspace_root,
        "x86_64-unknown-none",
        ARCEOS_QEMU_GUEST_PACKAGE,
        debug,
    )
}

pub(super) fn arceos_guest_elf_path(
    workspace_root: &Path,
    target: &str,
    package: &str,
    debug: bool,
) -> PathBuf {
    crate::backtrace::arceos_rust_elf_path(workspace_root, target, package, debug)
}

pub(super) fn arceos_x86_64_guest_bin_path(workspace_root: &Path) -> PathBuf {
    arceos_x86_64_guest_elf_path(workspace_root, false).with_extension("bin")
}

pub(super) fn inject_arceos_x86_64_guest_image(
    workspace_root: &Path,
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxvisorQemuCase,
    prepared_assets: &mut test_case::PreparedCaseAssets,
) -> anyhow::Result<()> {
    let guest_image = arceos_x86_64_guest_bin_path(workspace_root);
    ensure_file_exists(&guest_image, "ArceOS guest image")?;

    let mut temporary_overlay_run_dir = None;
    let overlay_dir = if prepared_assets.rootfs_copy_to_remove.is_none() {
        let layout = test_case::case_asset_layout(
            workspace_root,
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
            workspace_root,
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
    let result = crate::rootfs::inject::inject_overlay(&prepared_assets.rootfs_path, &overlay_dir);
    test_case::remove_case_run_dir(temporary_overlay_run_dir.as_deref());
    result
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

pub(super) fn build_group_needs_arceos_x86_64_guest(request: &ResolvedAxvisorRequest) -> bool {
    request.arch == "x86_64"
        && request.vmconfigs.iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("arceos"))
        })
}

fn matching_arceos_ivc_guest_profiles(
    request: &ResolvedAxvisorRequest,
) -> impl Iterator<Item = &'static ArceosIvcGuestProfile> + '_ {
    ARCEOS_IVC_GUEST_PROFILES
        .iter()
        .filter(|profile| profile.matches(request))
}

impl ArceosIvcGuestProfile {
    fn matches(&self, request: &ResolvedAxvisorRequest) -> bool {
        request.arch == self.arch
            && request
                .vmconfigs
                .iter()
                .any(|path| self.matches_vmconfig_path(path))
    }

    fn matches_vmconfig_path(&self, path: &Path) -> bool {
        path.file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(self.vmconfig_marker))
    }
}

pub(super) fn case_needs_arceos_x86_64_guest(
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxvisorQemuCase,
) -> bool {
    request.arch == "x86_64"
        && (build_group_needs_arceos_x86_64_guest(request)
            || case.case.case.name.contains("arceos"))
}

pub(super) fn axvisor_case_asset_config() -> test_case::CaseAssetConfig {
    test_case::CaseAssetConfig {
        grouped_runner: test_case::GroupedCaseRunnerConfig {
            runner_name: "axvisor-run-case-tests".to_string(),
            runner_path: "/usr/bin/axvisor-run-case-tests".to_string(),
            autorun_profile_script: None,
            begin_marker: "AXVISOR_GROUPED_TEST_BEGIN".to_string(),
            passed_marker: "AXVISOR_GROUPED_TEST_PASSED".to_string(),
            failed_marker: "AXVISOR_GROUPED_TEST_FAILED".to_string(),
            all_passed_marker: "AXVISOR_GROUPED_TESTS_PASSED".to_string(),
            all_failed_marker: "AXVISOR_GROUPED_TESTS_FAILED".to_string(),
            success_regex: r"(?m)^AXVISOR_GROUPED_TESTS_PASSED\s*$".to_string(),
            fail_regex: r"(?m)^AXVISOR_GROUPED_TEST_FAILED:".to_string(),
        },
        script_env: test_case::CaseScriptEnvConfig {
            staging_root: "AXVISOR_TEST_STAGING_ROOT".to_string(),
            case_dir: "AXVISOR_TEST_CASE_DIR".to_string(),
            case_c_dir: "AXVISOR_TEST_CASE_C_DIR".to_string(),
            case_work_dir: "AXVISOR_TEST_CASE_WORK_DIR".to_string(),
            case_build_dir: "AXVISOR_TEST_CASE_BUILD_DIR".to_string(),
            case_overlay_dir: "AXVISOR_TEST_CASE_OVERLAY_DIR".to_string(),
        },
        cache_env_vars: Vec::new(),
        prepare_staging_root: |_| Ok(()),
        prepare_guest_package_env: None,
    }
}
