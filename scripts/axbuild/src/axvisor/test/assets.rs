use std::{fs, io::Write, path::Path};

use anyhow::{Context, ensure};
use serde::Deserialize;

use super::types::PreparedAxvisorQemuCase;
use crate::{context::ResolvedAxvisorRequest, test::case as test_case};

const BUSYBOX_INITRAMFS_CONFIG: &str = "busybox-initramfs.toml";

#[derive(Deserialize)]
struct BusyboxInitramfsConfig {
    guest_path: String,
    #[serde(default = "default_busybox_path")]
    busybox_path: String,
    #[serde(default = "default_aarch64_musl_loader_path")]
    loader_path: String,
}

fn default_busybox_path() -> String {
    "/bin/busybox".to_string()
}

fn default_aarch64_musl_loader_path() -> String {
    "/lib/ld-musl-aarch64.so.1".to_string()
}

pub(super) fn inject_busybox_initramfs_if_requested(
    workspace_root: &Path,
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxvisorQemuCase,
    prepared_assets: &mut test_case::PreparedCaseAssets,
) -> anyhow::Result<()> {
    let config_path = case.case.case.case_dir.join(BUSYBOX_INITRAMFS_CONFIG);
    if !config_path.is_file() {
        return Ok(());
    }
    ensure!(
        request.arch == "aarch64",
        "{BUSYBOX_INITRAMFS_CONFIG} currently supports only aarch64 test cases"
    );
    let config: BusyboxInitramfsConfig = toml::from_str(
        &fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", config_path.display()))?;
    for (field, path) in [
        ("guest_path", config.guest_path.as_str()),
        ("busybox_path", config.busybox_path.as_str()),
        ("loader_path", config.loader_path.as_str()),
    ] {
        ensure!(path.starts_with('/'), "{field} must be an absolute path");
    }

    let layout = test_case::case_asset_layout(
        workspace_root,
        &request.target,
        &case.case.case.display_name,
    )?;
    fs::create_dir_all(&layout.run_dir)
        .with_context(|| format!("failed to create {}", layout.run_dir.display()))?;
    if prepared_assets.rootfs_copy_to_remove.is_none() {
        test_case::copy_shared_rootfs_for_case(&prepared_assets.rootfs_path, &layout)?;
        prepared_assets.rootfs_path = layout.case_rootfs_copy.clone();
        prepared_assets.rootfs_copy_to_remove = Some(layout.case_rootfs_copy.clone());
        prepared_assets.run_dir_to_remove = Some(layout.run_dir.clone());
    }

    let initramfs_dir = layout.run_dir.join("busybox-initramfs");
    fs::create_dir_all(&initramfs_dir)
        .with_context(|| format!("failed to create {}", initramfs_dir.display()))?;
    let busybox = initramfs_dir.join("busybox");
    let loader = initramfs_dir.join("ld-musl-aarch64.so.1");
    crate::rootfs::inject::extract_file(
        &prepared_assets.rootfs_path,
        &config.busybox_path,
        &busybox,
    )?;
    crate::rootfs::inject::extract_file(
        &prepared_assets.rootfs_path,
        &config.loader_path,
        &loader,
    )?;

    let archive = build_busybox_initramfs(&fs::read(busybox)?, &fs::read(loader)?)?;
    let archive_path = initramfs_dir.join("initramfs.cpio");
    fs::write(&archive_path, archive)
        .with_context(|| format!("failed to write {}", archive_path.display()))?;

    let overlay_dir = initramfs_dir.join("overlay");
    copy_guest_overlay_file(
        &archive_path,
        &overlay_dir,
        &config.guest_path,
        "BusyBox initramfs",
    )?;
    crate::rootfs::inject::inject_overlay(&prepared_assets.rootfs_path, &overlay_dir)
}

fn build_busybox_initramfs(busybox: &[u8], loader: &[u8]) -> anyhow::Result<Vec<u8>> {
    const INIT: &[u8] = b"#!/bin/busybox sh\n\
/bin/busybox mount -t devtmpfs devtmpfs /dev\n\
/bin/busybox mount -t proc proc /proc\n\
/bin/busybox mount -t sysfs sysfs /sys\n\
/bin/busybox printf 'AXVM> '\n\
IFS= read -r command\n\
/bin/busybox sh -c \"$command\"\n\
exec /bin/busybox sh\n";

    let mut archive = Vec::new();
    let mut inode = 1;
    for directory in ["bin", "dev", "lib", "proc", "sys", "tmp"] {
        append_newc_entry(&mut archive, inode, directory, 0o040755, &[])?;
        inode += 1;
    }
    for (name, mode, contents) in [
        ("init", 0o100755, INIT),
        ("bin/busybox", 0o100755, busybox),
        ("bin/sh", 0o120777, b"busybox".as_slice()),
        ("lib/ld-musl-aarch64.so.1", 0o100755, loader),
    ] {
        append_newc_entry(&mut archive, inode, name, mode, contents)?;
        inode += 1;
    }
    append_newc_entry(&mut archive, inode, "TRAILER!!!", 0, &[])?;
    archive.resize(archive.len().next_multiple_of(512), 0);
    Ok(archive)
}

fn append_newc_entry(
    archive: &mut Vec<u8>,
    inode: u32,
    name: &str,
    mode: u32,
    contents: &[u8],
) -> anyhow::Result<()> {
    ensure!(
        !name.is_empty() && !name.starts_with('/'),
        "invalid cpio entry name"
    );
    let name_size = name
        .len()
        .checked_add(1)
        .and_then(|size| u32::try_from(size).ok())
        .context("cpio entry name is too long")?;
    let file_size = u32::try_from(contents.len()).context("cpio entry is larger than 4 GiB")?;
    write!(
        archive,
        "070701{inode:08x}{mode:08x}{:08x}{:08x}{:08x}{:08x}{file_size:08x}{:08x}{:08x}{:08x}{:\
         08x}{name_size:08x}{:08x}",
        0, 0, 1, 0, 0, 0, 0, 0, 0
    )?;
    archive.extend_from_slice(name.as_bytes());
    archive.push(0);
    archive.resize(archive.len().next_multiple_of(4), 0);
    archive.extend_from_slice(contents);
    archive.resize(archive.len().next_multiple_of(4), 0);
    Ok(())
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
    use super::*;

    #[test]
    fn busybox_initramfs_is_newc_aligned_and_contains_init() {
        let archive = build_busybox_initramfs(b"busybox", b"loader").unwrap();

        assert_eq!(&archive[..6], b"070701");
        assert_eq!(archive.len() % 512, 0);
        assert!(archive.windows(5).any(|bytes| bytes == b"init\0"));
        assert!(archive.windows(11).any(|bytes| bytes == b"TRAILER!!!\0"));
    }
}
