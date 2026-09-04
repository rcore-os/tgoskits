use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;

use super::{discovery::ensure_file_exists, types::PreparedAxvisorQemuCase};
use crate::{
    context::{ResolvedAxvisorRequest, ResolvedBuildRequest},
    test::case as test_case,
};

const ARCEOS_QEMU_GUEST_PACKAGE: &str = "ax-helloworld";
const ARCEOS_QEMU_GUEST_KERNEL_PATH: &str = "/guest/arceos/ax-helloworld-x86_64.bin";

pub(super) fn arceos_x86_64_guest_request() -> anyhow::Result<ResolvedBuildRequest> {
    let target = "x86_64-unknown-none".to_string();
    Ok(ResolvedBuildRequest {
        package: ARCEOS_QEMU_GUEST_PACKAGE.to_string(),
        arch: "x86_64".to_string(),
        target: target.clone(),
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

pub(super) fn arceos_x86_64_guest_elf_path(workspace_root: &Path, debug: bool) -> PathBuf {
    crate::backtrace::arceos_rust_elf_path(
        workspace_root,
        "x86_64-unknown-none",
        ARCEOS_QEMU_GUEST_PACKAGE,
        debug,
    )
}

pub(super) fn arceos_x86_64_guest_bin_path(workspace_root: &Path) -> PathBuf {
    arceos_x86_64_guest_elf_path(workspace_root, false).with_extension("bin")
}

const ARCEOS_AARCH64_SMOKE_IMAGE_BUNDLE: &str = "qemu-aarch64";
const ARCEOS_AARCH64_SMOKE_GUEST_KERNEL: &str = "arceos/arceos-qemu";
const ARCEOS_RT_LATENCY_GUEST_IMAGE: &str = "qemu-aarch64-rt-latency-bench";
const ARCEOS_RT_LATENCY_GUEST_BUILD_SCRIPT: &str =
    "os/axvisor/scripts/task1/build-arceos-rt-guest.sh";

pub(super) fn arceos_aarch64_smoke_guest_image_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(format!(
        "os/axvisor/images/{ARCEOS_AARCH64_SMOKE_IMAGE_BUNDLE}/{ARCEOS_AARCH64_SMOKE_GUEST_KERNEL}"
    ))
}

pub(super) fn vmconfigs_need_arceos_aarch64_smoke_guest(
    request: &ResolvedAxvisorRequest,
    vmconfigs: &[PathBuf],
) -> bool {
    request.arch == "aarch64"
        && vmconfigs.iter().any(|path| {
            path.to_string_lossy().contains("arceos-rt-smp1-smoke")
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("arceos-rt-smp1-smoke"))
        })
}

pub(super) fn ensure_arceos_aarch64_smoke_guest_image(workspace_root: &Path) -> anyhow::Result<()> {
    let image_path = arceos_aarch64_smoke_guest_image_path(workspace_root);
    if image_path.is_file() {
        return Ok(());
    }

    if let Some(parent) = image_path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create ArceOS smoke guest image directory {}",
                parent.display()
            )
        })?;
    }

    let output_dir = workspace_root.join("os/axvisor/images");
    let status = Command::new("cargo")
        .args([
            "xtask",
            "image",
            "pull",
            ARCEOS_AARCH64_SMOKE_IMAGE_BUNDLE,
            "--output-dir",
        ])
        .arg(&output_dir)
        .current_dir(workspace_root)
        .status()
        .context("failed to spawn `cargo xtask image pull` for ArceOS smoke guest")?;
    if !status.success() {
        anyhow::bail!("`cargo xtask image pull {ARCEOS_AARCH64_SMOKE_IMAGE_BUNDLE}` failed");
    }

    ensure_file_exists(&image_path, "ArceOS smoke guest image")?;
    Ok(())
}

pub(super) fn arceos_rt_latency_guest_image_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(format!(
        "os/axvisor/images/qemu_aarch64_arceos_rt/{ARCEOS_RT_LATENCY_GUEST_IMAGE}"
    ))
}

pub(super) fn vmconfig_requests_rt_latency_guest_image(contents: &str) -> bool {
    contents.contains(ARCEOS_RT_LATENCY_GUEST_IMAGE)
}

pub(super) fn vmconfigs_need_arceos_rt_latency_guest(
    workspace_root: &Path,
    vmconfigs: &[PathBuf],
) -> bool {
    vmconfigs.iter().any(|path| {
        let path = if path.is_absolute() {
            path.clone()
        } else {
            workspace_root.join(path)
        };
        fs::read_to_string(&path)
            .is_ok_and(|contents| vmconfig_requests_rt_latency_guest_image(&contents))
    })
}

pub(super) fn ensure_arceos_rt_latency_guest_image(workspace_root: &Path) -> anyhow::Result<()> {
    let image_path = arceos_rt_latency_guest_image_path(workspace_root);
    if image_path.is_file() {
        return Ok(());
    }

    let script = workspace_root.join(ARCEOS_RT_LATENCY_GUEST_BUILD_SCRIPT);
    if !script.is_file() {
        anyhow::bail!(
            "missing ArceOS rt-latency guest build script `{}`",
            script.display()
        );
    }
    let status = Command::new(&script)
        .current_dir(workspace_root)
        .status()
        .context("failed to spawn ArceOS rt-latency guest build script")?;
    if !status.success() {
        anyhow::bail!(
            "ArceOS rt-latency guest build `{}` failed (exit={})",
            script.display(),
            status.code().unwrap_or(-1)
        );
    }

    ensure_file_exists(&image_path, "ArceOS rt-latency guest image")?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{vmconfig_requests_rt_latency_guest_image, vmconfigs_need_arceos_rt_latency_guest};

    #[test]
    fn rt_smp1_memory_image_is_the_custom_latency_bench() {
        assert!(vmconfig_requests_rt_latency_guest_image(
            r#"kernel_path = "../../../../images/qemu_aarch64_arceos_rt/qemu-aarch64-rt-latency-bench""#
        ));
        assert!(!vmconfig_requests_rt_latency_guest_image(
            r#"kernel_path = "../../../../images/qemu-aarch64/arceos/arceos-qemu""#
        ));
    }

    #[test]
    fn missing_rt_latency_bench_vmconfig_does_not_request_custom_guest() {
        let dir = tempfile::tempdir().unwrap();
        let smoke = dir.path().join("arceos-rt-smp1-smoke.toml");
        let guest = dir.path().join("arceos-rt-smp1.toml");
        fs::write(
            &smoke,
            r#"kernel_path = "../../../../images/qemu-aarch64/arceos/arceos-qemu""#,
        )
        .unwrap();
        fs::write(
            &guest,
            r#"kernel_path = "../../../../images/qemu_aarch64_arceos_rt/qemu-aarch64-rt-latency-bench""#,
        )
        .unwrap();
        assert!(!vmconfigs_need_arceos_rt_latency_guest(
            dir.path(),
            &[smoke]
        ));
        assert!(vmconfigs_need_arceos_rt_latency_guest(dir.path(), &[guest]));
        assert!(!vmconfigs_need_arceos_rt_latency_guest(
            dir.path(),
            &[PathBuf::from("missing.toml")]
        ));
    }
}
