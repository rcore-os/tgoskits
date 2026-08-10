use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Context;

use super::{discovery::ensure_file_exists, types::PreparedAxvisorQemuCase};
use crate::{
    context::ResolvedAxvisorRequest,
    test::case as test_case,
};

const ARCEOS_QEMU_GUEST_PACKAGE: &str = "ax-helloworld";
const ARCEOS_QEMU_GUEST_KERNEL_PATH: &str = "/guest/arceos/ax-helloworld-x86_64.bin";
const AXVISOR_IVSHMEM_BAR2_SMOKE_GUEST_PATH: &str = "/root/ivshmem-bar2-smoke";
const AXVISOR_IVSHMEM_BAR2_INITRAMFS_GUEST_PATH: &str = "/guest/linux/ivshmem-bar2-initramfs.cpio";
const AXVISOR_IVSHMEM_ZEPHYR_GUEST_PATH: &str = "/guest/zephyr/zephyr-ivshmem-peer.bin";

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

pub(super) fn inject_linux_ivshmem_assets(
    workspace_root: &Path,
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxvisorQemuCase,
    prepared_assets: &mut test_case::PreparedCaseAssets,
) -> anyhow::Result<()> {
    if !case_needs_linux_ivshmem_assets(request, case) {
        return Ok(());
    }

    let out_dir = build_linux_ivshmem_assets(workspace_root, &request.arch)?;
    let smoke = out_dir.join("ivshmem-bar2-smoke");
    let initramfs = out_dir.join("ivshmem-bar2-initramfs.cpio");
    ensure_file_exists(&smoke, "Linux ivshmem BAR2 smoke test")?;
    ensure_file_exists(&initramfs, "Linux ivshmem BAR2 initramfs")?;
    let zephyr = case_needs_zephyr_ivshmem_assets(request, case)
        .then(|| build_zephyr_ivshmem_peer(workspace_root))
        .transpose()?;
    if let Some(zephyr) = &zephyr {
        ensure_file_exists(zephyr, "Zephyr ivshmem peer image")?;
    }

    let (overlay_dir, temporary_overlay_run_dir) =
        direct_overlay_dir(workspace_root, request, case)?;
    copy_guest_overlay_file(
        &smoke,
        &overlay_dir,
        AXVISOR_IVSHMEM_BAR2_SMOKE_GUEST_PATH,
        "Linux ivshmem BAR2 smoke test",
    )?;
    copy_guest_overlay_file(
        &initramfs,
        &overlay_dir,
        AXVISOR_IVSHMEM_BAR2_INITRAMFS_GUEST_PATH,
        "Linux ivshmem BAR2 initramfs",
    )?;
    if let Some(zephyr) = zephyr {
        copy_guest_overlay_file(
            &zephyr,
            &overlay_dir,
            AXVISOR_IVSHMEM_ZEPHYR_GUEST_PATH,
            "Zephyr ivshmem peer image",
        )?;
    }
    let result = crate::rootfs::inject::inject_overlay(&prepared_assets.rootfs_path, &overlay_dir);
    test_case::remove_case_run_dir(temporary_overlay_run_dir.as_deref());
    result
}

fn direct_overlay_dir(
    workspace_root: &Path,
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxvisorQemuCase,
) -> anyhow::Result<(PathBuf, Option<PathBuf>)> {
    let layout = test_case::case_asset_layout(
        workspace_root,
        &request.target,
        &case.case.case.display_name,
    )?;
    fs::create_dir_all(&layout.run_dir)
        .with_context(|| format!("failed to create {}", layout.run_dir.display()))?;
    Ok((layout.overlay_dir, Some(layout.run_dir)))
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

fn case_needs_linux_ivshmem_assets(
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxvisorQemuCase,
) -> bool {
    case.case.case.name.contains("ivshmem")
        && request.vmconfigs.iter().any(|path| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("linux-ivshmem"))
        })
}

fn case_needs_zephyr_ivshmem_assets(
    request: &ResolvedAxvisorRequest,
    case: &PreparedAxvisorQemuCase,
) -> bool {
    case.case.case.name.contains("ivshmem")
        && request.vmconfigs.iter().any(|path| {
            path.file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains("zephyr-ivshmem"))
        })
}

fn build_linux_ivshmem_assets(workspace_root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let source_dir = workspace_root.join("apps/linux/ivshmem");
    let build_script = source_dir.join("build.sh");
    ensure_file_exists(&build_script, "Linux ivshmem build script")?;

    let out_dir = workspace_root.join("tmp/axbuild/ivshmem").join(arch);
    let mut command = Command::new(&build_script);
    command
        .current_dir(&source_dir)
        .env("AXVISOR_IVSHMEM_ARCH", arch)
        .env("AXVISOR_IVSHMEM_OUT_DIR", &out_dir);

    let status = command
        .status()
        .with_context(|| format!("failed to run {}", build_script.display()))?;
    if !status.success() {
        anyhow::bail!("Linux ivshmem asset build failed with status {status}");
    }
    write_ivshmem_bar2_initramfs(
        &out_dir.join("ivshmem-bar2-initramfs.cpio"),
        &out_dir.join("ivshmem-bar2-smoke"),
    )?;
    Ok(out_dir)
}

fn build_zephyr_ivshmem_peer(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let source_dir = workspace_root.join("apps/zephyr/ivshmem_peer");
    let build_script = source_dir.join("build.sh");
    ensure_file_exists(&build_script, "Zephyr ivshmem build script")?;

    let out_dir = workspace_root.join("tmp/axbuild/ivshmem/zephyr");
    let mut command = Command::new(&build_script);
    command
        .current_dir(&source_dir)
        .env("AXVISOR_ZEPHYR_IVSHMEM_OUT_DIR", &out_dir);

    let status = command
        .status()
        .with_context(|| format!("failed to run {}", build_script.display()))?;
    if !status.success() {
        anyhow::bail!("Zephyr ivshmem peer build failed with status {status}");
    }
    Ok(out_dir.join("zephyr-ivshmem-peer.bin"))
}

fn write_ivshmem_bar2_initramfs(output: &Path, init_binary: &Path) -> anyhow::Result<()> {
    let init = fs::read(init_binary)
        .with_context(|| format!("failed to read {}", init_binary.display()))?;
    let mut archive = Vec::new();

    append_cpio_newc_entry(&mut archive, ".", 0o040755, &[], 1);
    append_cpio_newc_entry(&mut archive, "init", 0o100755, &init, 2);
    append_cpio_newc_entry(&mut archive, "TRAILER!!!", 0, &[], 3);

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(output, archive).with_context(|| format!("failed to write {}", output.display()))
}

fn append_cpio_newc_entry(
    archive: &mut Vec<u8>,
    name: &str,
    mode: u32,
    content: &[u8],
    inode: u32,
) {
    let namesize = name.len() + 1;
    let header = format!(
        "070701{inode:08x}{mode:08x}{uid:08x}{gid:08x}{nlink:08x}{mtime:08x}{filesize:\
         08x}{devmajor:08x}{devminor:08x}{rdevmajor:08x}{rdevminor:08x}{namesize:08x}{check:08x}",
        uid = 0,
        gid = 0,
        nlink = 1,
        mtime = 0,
        filesize = content.len(),
        devmajor = 0,
        devminor = 0,
        rdevmajor = 0,
        rdevminor = 0,
        check = 0,
    );
    archive.extend_from_slice(header.as_bytes());
    archive.extend_from_slice(name.as_bytes());
    archive.push(0);
    pad_cpio_newc(archive);
    archive.extend_from_slice(content);
    pad_cpio_newc(archive);
}

fn pad_cpio_newc(archive: &mut Vec<u8>) {
    while archive.len() % 4 != 0 {
        archive.push(0);
    }
}

pub(super) fn build_group_needs_arceos_x86_64_guest(request: &ResolvedAxvisorRequest) -> bool {
    request.arch == "x86_64"
        && request.vmconfigs.iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("arceos") && !name.contains("ivc"))
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
