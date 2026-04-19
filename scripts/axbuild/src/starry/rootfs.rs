use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    net::IpAddr,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail, ensure};
use indicatif::ProgressBar;
use ostool::run::qemu::QemuConfig;
use tokio::fs as tokio_fs;
use xz2::read::XzDecoder;

use super::test_suit::StarryQemuCase;
use crate::{
    context::{ResolvedStarryRequest, starry_target_for_arch_checked},
    download::download_to_path_with_progress,
    process::ProcessExt,
};

const ROOTFS_URL: &str = "https://github.com/Starry-OS/rootfs/releases/download/20260214";
const CASE_WORK_ROOT_NAME: &str = "starry-cases";
const CASE_STAGING_DIR_NAME: &str = "staging-root";
const CASE_BUILD_DIR_NAME: &str = "build";
const CASE_OVERLAY_DIR_NAME: &str = "overlay";
const CASE_COMMAND_WRAPPER_DIR_NAME: &str = "guest-bin";
const CASE_CROSS_BIN_DIR_NAME: &str = "cross-bin";
const CASE_CMAKE_TOOLCHAIN_FILE_NAME: &str = "cmake-toolchain.cmake";
const CASE_APK_CACHE_DIR_NAME: &str = "apk-cache";
const CASE_C_DIR_NAME: &str = "c";
const CASE_PREBUILD_SCRIPT_NAME: &str = "prebuild.sh";
const CASE_CMAKE_FILE_NAME: &str = "CMakeLists.txt";
const STARRY_APK_REGION_VAR: &str = "STARRY_APK_REGION";
const STARRY_STAGING_ROOT_VAR: &str = "STARRY_STAGING_ROOT";
const STARRY_CASE_DIR_VAR: &str = "STARRY_CASE_DIR";
const STARRY_CASE_C_DIR_VAR: &str = "STARRY_CASE_C_DIR";
const STARRY_CASE_WORK_DIR_VAR: &str = "STARRY_CASE_WORK_DIR";
const STARRY_CASE_BUILD_DIR_VAR: &str = "STARRY_CASE_BUILD_DIR";
const STARRY_CASE_OVERLAY_DIR_VAR: &str = "STARRY_CASE_OVERLAY_DIR";
const HOST_RESOLV_CONF_PATH: &str = "/etc/resolv.conf";
const HOST_RESOLVED_CONF_PATH: &str = "/run/systemd/resolve/resolv.conf";
const DEFAULT_DNS_SERVERS: &[&str] = &["1.1.1.1", "8.8.8.8"];
const CROSS_BINUTILS: &[&str] = &[
    "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
];
const RUNTIME_LIBRARY_DIRS: &[&str] = &["lib", "usr/lib", "usr/local/lib"];
const CHINA_ALPINE_MIRROR: &str = "https://mirrors.cernet.edu.cn/alpine";
const US_ALPINE_MIRROR: &str = "https://dl-cdn.alpinelinux.org/alpine";
const USB_STICK_IMAGE_NAME: &str = "usb-stick.raw";
const USB_STICK_IMAGE_SIZE: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryCaseAssets {
    pub(crate) rootfs_path: PathBuf,
    pub(crate) extra_qemu_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CaseAssetLayout {
    work_dir: PathBuf,
    staging_root: PathBuf,
    build_dir: PathBuf,
    overlay_dir: PathBuf,
    command_wrapper_dir: PathBuf,
    cross_bin_dir: PathBuf,
    cmake_toolchain_file: PathBuf,
    apk_cache_dir: PathBuf,
    usb_stick_path: PathBuf,
}

#[derive(Debug, Clone)]
struct HostCrossBuildEnv {
    cmake: PathBuf,
    pkg_config: PathBuf,
    make_program: PathBuf,
    cmake_toolchain_file: PathBuf,
    command_envs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrossCompileSpec {
    llvm_target: &'static str,
    cmake_system_processor: &'static str,
    guest_tool_dir: &'static str,
    gnu_tool_prefix: &'static str,
}

#[derive(Debug, Clone)]
struct GuestPrebuildEnv {
    qemu_runner: PathBuf,
    script_envs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApkRegion {
    China,
    Us,
}

impl ApkRegion {
    fn canonical_name(self) -> &'static str {
        match self {
            Self::China => "china",
            Self::Us => "us",
        }
    }

    fn mirror_base(self) -> &'static str {
        match self {
            Self::China => CHINA_ALPINE_MIRROR,
            Self::Us => US_ALPINE_MIRROR,
        }
    }
}

pub(crate) fn rootfs_image_name(arch: &str) -> anyhow::Result<String> {
    let _ = starry_target_for_arch_checked(arch)?;
    Ok(format!("rootfs-{arch}.img"))
}

pub(crate) fn resolve_target_dir(workspace_root: &Path, target: &str) -> anyhow::Result<PathBuf> {
    let _ = crate::context::starry_arch_for_target_checked(target)?;
    Ok(workspace_root.join("target").join(target))
}

fn rootfs_image_path(workspace_root: &Path, arch: &str, target: &str) -> anyhow::Result<PathBuf> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    Ok(target_dir.join(rootfs_image_name(arch)?))
}

pub(crate) async fn ensure_rootfs_in_target_dir(
    workspace_root: &Path,
    arch: &str,
    target: &str,
) -> anyhow::Result<PathBuf> {
    let expected_target = starry_target_for_arch_checked(arch)?;
    if target != expected_target {
        bail!("Starry arch `{arch}` maps to target `{expected_target}`, but got `{target}`");
    }

    let target_dir = resolve_target_dir(workspace_root, target)?;
    tokio_fs::create_dir_all(&target_dir)
        .await
        .with_context(|| format!("failed to create {}", target_dir.display()))?;

    let rootfs_name = rootfs_image_name(arch)?;
    let rootfs_img = rootfs_image_path(workspace_root, arch, target)?;
    let rootfs_xz = target_dir.join(format!("{rootfs_name}.xz"));

    if !rootfs_img.exists() {
        println!("image not found, downloading {}...", rootfs_name);
        let url = format!("{ROOTFS_URL}/{rootfs_name}.xz");
        download_with_progress(&url, &rootfs_xz).await?;
        decompress_xz_file(&rootfs_xz, &rootfs_img).await?;
    }

    Ok(rootfs_img)
}

fn per_case_rootfs_path(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case_name: &str,
) -> anyhow::Result<PathBuf> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    Ok(target_dir.join(format!("rootfs-{arch}-{case_name}.img")))
}

pub(crate) async fn prepare_per_case_rootfs(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case_name: &str,
) -> anyhow::Result<PathBuf> {
    let base = rootfs_image_path(workspace_root, arch, target)?;
    let case_rootfs = per_case_rootfs_path(workspace_root, arch, target, case_name)?;

    if case_rootfs.exists() {
        tokio_fs::remove_file(&case_rootfs).await.with_context(|| {
            format!(
                "failed to remove old per-case rootfs {}",
                case_rootfs.display()
            )
        })?;
    }

    let src = base.clone();
    let dst = case_rootfs.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::copy(&src, &dst)
            .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("rootfs copy task failed")??;

    Ok(case_rootfs)
}

pub(crate) async fn prepare_case_assets(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryQemuCase,
) -> anyhow::Result<StarryCaseAssets> {
    run_case_build_script(&case.case_dir, workspace_root, arch, target).await?;

    let case_rootfs = prepare_per_case_rootfs(workspace_root, arch, target, &case.name).await?;
    let rootfs_files_dir = case.case_dir.join("rootfs-files");
    if rootfs_files_dir.is_dir() {
        let case_rootfs_for_task = case_rootfs.clone();
        tokio::task::spawn_blocking(move || {
            inject_rootfs_tree(&case_rootfs_for_task, &rootfs_files_dir)
        })
        .await
        .context("rootfs injection task failed")??;
    }

    let needs_assets = case_uses_c_pipeline(case) || case_uses_usb_qemu_assets(arch, case);

    if !needs_assets {
        return Ok(StarryCaseAssets {
            rootfs_path: case_rootfs,
            extra_qemu_args: Vec::new(),
        });
    }

    let workspace_root = workspace_root.to_path_buf();
    let arch = arch.to_string();
    let target = target.to_string();
    let case_rootfs_for_task = case_rootfs.clone();
    let case = case.clone();
    let extra_qemu_args = tokio::task::spawn_blocking(move || {
        prepare_case_assets_sync(
            &workspace_root,
            &arch,
            &target,
            &case,
            &case_rootfs_for_task,
        )
    })
    .await
    .context("starry case asset task failed")??;

    Ok(StarryCaseAssets {
        rootfs_path: case_rootfs,
        extra_qemu_args,
    })
}

pub(crate) async fn apply_default_qemu_args(
    workspace_root: &Path,
    request: &ResolvedStarryRequest,
    qemu: &mut QemuConfig,
) -> anyhow::Result<()> {
    let disk_img =
        ensure_rootfs_in_target_dir(workspace_root, &request.arch, &request.target).await?;
    apply_disk_image_qemu_args(qemu, disk_img);
    Ok(())
}

pub(crate) fn apply_smp_qemu_arg(qemu: &mut QemuConfig, smp: Option<usize>) {
    let Some(cpu_num) = smp else {
        return;
    };

    if let Some(index) = qemu.args.iter().position(|arg| arg == "-smp")
        && let Some(value) = qemu.args.get_mut(index + 1)
    {
        *value = cpu_num.to_string();
        return;
    }

    qemu.args.push("-smp".to_string());
    qemu.args.push(cpu_num.to_string());
}

pub(crate) fn smp_from_qemu_arg(qemu: &QemuConfig) -> Option<usize> {
    let index = qemu.args.iter().position(|arg| arg == "-smp")?;
    let value = qemu.args.get(index + 1)?;
    parse_smp_qemu_value(value)
}

fn parse_smp_qemu_value(value: &str) -> Option<usize> {
    let first = value.split(',').next()?;
    if let Ok(cpu_num) = first.parse() {
        return Some(cpu_num);
    }

    value.split(',').find_map(|part| {
        let cpu_num = part.strip_prefix("cpus=")?;
        cpu_num.parse().ok()
    })
}

pub(crate) fn apply_disk_image_qemu_args(qemu: &mut QemuConfig, disk_img: PathBuf) {
    let disk_value = format!("id=disk0,if=none,format=raw,file={}", disk_img.display());
    let args = &mut qemu.args;

    let mut has_blk_device = false;
    let mut has_drive = false;
    let mut has_net_device = false;
    let mut has_netdev = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-device" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value == "virtio-blk-pci,drive=disk0" {
                    has_blk_device = true;
                } else if value == "virtio-net-pci,netdev=net0" {
                    has_net_device = true;
                }
                index += 2;
            }
            "-drive" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value.starts_with("id=disk0,if=none,format=raw,file=") {
                    *value = disk_value.clone();
                    has_drive = true;
                }
                index += 2;
            }
            "-netdev" if index + 1 < args.len() => {
                let value = &mut args[index + 1];
                if value == "user,id=net0" {
                    has_netdev = true;
                }
                index += 2;
            }
            _ => index += 1,
        }
    }

    if !has_blk_device {
        args.push("-device".to_string());
        args.push("virtio-blk-pci,drive=disk0".to_string());
    }
    if !has_drive {
        args.push("-drive".to_string());
        args.push(disk_value);
    }
    if !has_net_device {
        args.push("-device".to_string());
        args.push("virtio-net-pci,netdev=net0".to_string());
    }
    if !has_netdev {
        args.push("-netdev".to_string());
        args.push("user,id=net0".to_string());
    }
}

async fn download_with_progress(url: &str, output_path: &Path) -> anyhow::Result<()> {
    let client = crate::download::http_client()?;
    download_to_path_with_progress(&client, url, output_path).await
}

fn case_uses_c_pipeline(case: &StarryQemuCase) -> bool {
    case_c_source_dir(case).is_dir()
}

fn case_uses_usb_qemu_assets(arch: &str, case: &StarryQemuCase) -> bool {
    let _ = arch;
    let _ = case;
    false
}

fn case_c_source_dir(case: &StarryQemuCase) -> PathBuf {
    case.case_dir.join(CASE_C_DIR_NAME)
}

fn case_prebuild_script_path(case: &StarryQemuCase) -> PathBuf {
    case_c_source_dir(case).join(CASE_PREBUILD_SCRIPT_NAME)
}

fn case_asset_layout(
    workspace_root: &Path,
    target: &str,
    case_name: &str,
) -> anyhow::Result<CaseAssetLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(CASE_WORK_ROOT_NAME).join(case_name);

    Ok(CaseAssetLayout {
        staging_root: work_dir.join(CASE_STAGING_DIR_NAME),
        build_dir: work_dir.join(CASE_BUILD_DIR_NAME),
        overlay_dir: work_dir.join(CASE_OVERLAY_DIR_NAME),
        command_wrapper_dir: work_dir.join(CASE_COMMAND_WRAPPER_DIR_NAME),
        cross_bin_dir: work_dir.join(CASE_CROSS_BIN_DIR_NAME),
        cmake_toolchain_file: work_dir.join(CASE_CMAKE_TOOLCHAIN_FILE_NAME),
        apk_cache_dir: work_dir.join(CASE_APK_CACHE_DIR_NAME),
        usb_stick_path: work_dir.join(USB_STICK_IMAGE_NAME),
        work_dir,
    })
}

fn usb_qemu_args(usb_stick_path: &Path) -> Vec<String> {
    vec![
        "-device".to_string(),
        "qemu-xhci,id=xhci".to_string(),
        "-drive".to_string(),
        format!(
            "if=none,format=raw,file={},id=usbstick0",
            usb_stick_path.display()
        ),
        "-device".to_string(),
        "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
    ]
}

fn prepare_case_assets_sync(
    workspace_root: &Path,
    arch: &str,
    target: &str,
    case: &StarryQemuCase,
    case_rootfs: &Path,
) -> anyhow::Result<Vec<String>> {
    let layout = case_asset_layout(workspace_root, target, &case.name)?;
    fs::create_dir_all(&layout.work_dir)
        .with_context(|| format!("failed to create {}", layout.work_dir.display()))?;

    if case_uses_c_pipeline(case) {
        prepare_c_case_assets_sync(arch, case, case_rootfs, &layout)?;
    }

    let mut extra_qemu_args = Vec::new();
    if case_uses_usb_qemu_assets(arch, case) {
        create_usb_backing_image(&layout.usb_stick_path)?;
        extra_qemu_args.extend(usb_qemu_args(&layout.usb_stick_path));
    }

    Ok(extra_qemu_args)
}

fn prepare_c_case_assets_sync(
    arch: &str,
    case: &StarryQemuCase,
    case_rootfs: &Path,
    layout: &CaseAssetLayout,
) -> anyhow::Result<()> {
    let source_dir = case_c_source_dir(case);
    let cmake_lists = source_dir.join(CASE_CMAKE_FILE_NAME);
    ensure!(
        cmake_lists.is_file(),
        "missing case CMake project entry `{}`",
        cmake_lists.display()
    );

    reset_dir(&layout.staging_root)?;
    reset_dir(&layout.build_dir)?;
    reset_dir(&layout.overlay_dir)?;
    reset_dir(&layout.command_wrapper_dir)?;
    reset_dir(&layout.cross_bin_dir)?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;

    populate_staging_root(case_rootfs, &layout.staging_root)?;
    write_host_resolver_config(&layout.staging_root)?;
    let prebuild_script = case_prebuild_script_path(case);
    if prebuild_script.is_file() {
        let apk_region = apk_region_from_env()?;
        rewrite_apk_repositories_for_region(&layout.staging_root, apk_region)?;
        log_apk_prebuild_context(&layout.staging_root, apk_region)?;
        let prebuild_env = prepare_guest_prebuild_env(arch, case, layout, apk_region)?;
        let mut command = build_prebuild_command(case, &prebuild_script, layout, &prebuild_env)?;
        command.exec().context("failed to run case prebuild.sh")?;
    }
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    let build_env = prepare_host_cross_build_env(arch, layout, &qemu_runner)?;

    let mut configure = build_cmake_configure_command(case, layout, &build_env);
    configure
        .exec()
        .context("failed to configure case C project")?;

    let mut build = build_cmake_build_command(layout, &build_env);
    build.exec().context("failed to build case C project")?;

    let mut install = build_cmake_install_command(layout, &build_env);
    install.exec().context("failed to install case C project")?;

    sync_runtime_dependencies(&layout.staging_root, &layout.overlay_dir)?;
    inject_overlay_tree(case_rootfs, &layout.overlay_dir)
}

fn reset_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}

fn populate_staging_root(rootfs_img: &Path, staging_root: &Path) -> anyhow::Result<()> {
    Command::new("debugfs")
        .arg("-R")
        .arg(format!("rdump / {}", staging_root.display()))
        .arg(rootfs_img)
        .exec()
        .with_context(|| {
            format!(
                "failed to extract {} into {}",
                rootfs_img.display(),
                staging_root.display()
            )
        })
}

fn write_host_resolver_config(staging_root: &Path) -> anyhow::Result<()> {
    let resolv_conf = preferred_host_resolver_config()?;
    let output_path = staging_root.join("etc/resolv.conf");
    fs::write(&output_path, resolv_conf)
        .with_context(|| format!("failed to write {}", output_path.display()))
}

fn preferred_host_resolver_config() -> anyhow::Result<String> {
    if let Some(content) = read_usable_resolver_file(Path::new(HOST_RESOLVED_CONF_PATH))? {
        return Ok(content);
    }
    if let Some(content) = read_usable_resolver_file(Path::new(HOST_RESOLV_CONF_PATH))? {
        return Ok(content);
    }

    Ok(DEFAULT_DNS_SERVERS
        .iter()
        .map(|server| format!("nameserver {server}"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

fn read_usable_resolver_file(path: &Path) -> anyhow::Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }

    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let usable = content
        .lines()
        .filter_map(parse_nameserver_line)
        .filter(|addr| !addr.is_loopback() && *addr != IpAddr::from([10, 0, 2, 3]))
        .map(|addr| format!("nameserver {addr}"))
        .collect::<Vec<_>>();

    if usable.is_empty() {
        Ok(None)
    } else {
        Ok(Some(usable.join("\n") + "\n"))
    }
}

fn parse_nameserver_line(line: &str) -> Option<IpAddr> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut parts = trimmed.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some("nameserver"), Some(value), None) => value.parse().ok(),
        _ => None,
    }
}

fn apk_region_from_env() -> anyhow::Result<ApkRegion> {
    let value = std::env::var(STARRY_APK_REGION_VAR).ok();
    parse_apk_region(value.as_deref())
}

fn parse_apk_region(value: Option<&str>) -> anyhow::Result<ApkRegion> {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
    {
        None => Ok(ApkRegion::China),
        Some(value) if matches!(value.as_str(), "china" | "cn") => Ok(ApkRegion::China),
        Some(value) if matches!(value.as_str(), "us" | "usa") => Ok(ApkRegion::Us),
        Some(value) => bail!(
            "unsupported {STARRY_APK_REGION_VAR} `{value}`; supported values are: china, cn, us, \
             usa"
        ),
    }
}

fn rewrite_apk_repositories_for_region(
    staging_root: &Path,
    region: ApkRegion,
) -> anyhow::Result<()> {
    let path = staging_root.join("etc/apk/repositories");
    let original =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let ends_with_newline = original.ends_with('\n');
    let rewritten = original
        .lines()
        .map(|line| rewrite_apk_repository_line(line, region))
        .collect::<Vec<_>>()
        .join("\n");

    let mut output = rewritten;
    if ends_with_newline {
        output.push('\n');
    }

    fs::write(&path, output).with_context(|| format!("failed to write {}", path.display()))
}

fn rewrite_apk_repository_line(line: &str, region: ApkRegion) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return line.to_string();
    }

    let Some((_, suffix)) = trimmed.split_once("/alpine/") else {
        return line.to_string();
    };

    let leading_len = line.len() - line.trim_start().len();
    let trailing_len = line.len() - line.trim_end().len();
    let trailing = if trailing_len == 0 {
        ""
    } else {
        &line[line.len() - trailing_len..]
    };

    format!(
        "{}{}/{}{}",
        &line[..leading_len],
        region.mirror_base(),
        suffix,
        trailing
    )
}

fn log_apk_prebuild_context(staging_root: &Path, region: ApkRegion) -> anyhow::Result<()> {
    let repositories_path = staging_root.join("etc/apk/repositories");
    let repositories = fs::read_to_string(&repositories_path)
        .with_context(|| format!("failed to read {}", repositories_path.display()))?;

    println!("STARRY_APK_REGION={}", region.canonical_name());
    println!("apk repositories:");
    print!("{repositories}");
    if !repositories.ends_with('\n') {
        println!();
    }

    Ok(())
}

fn prepare_guest_prebuild_env(
    arch: &str,
    case: &StarryQemuCase,
    layout: &CaseAssetLayout,
    apk_region: ApkRegion,
) -> anyhow::Result<GuestPrebuildEnv> {
    let qemu_runner = find_host_binary_candidates(qemu_user_binary_names(arch)?)?;
    write_guest_command_wrappers(layout, &qemu_runner)?;

    let mut script_envs = case_script_envs(case, layout);
    script_envs.push((
        STARRY_APK_REGION_VAR.to_string(),
        apk_region.canonical_name().to_string(),
    ));

    Ok(GuestPrebuildEnv {
        qemu_runner,
        script_envs,
    })
}

fn prepare_host_cross_build_env(
    arch: &str,
    layout: &CaseAssetLayout,
    qemu_runner: &Path,
) -> anyhow::Result<HostCrossBuildEnv> {
    let spec = cross_compile_spec(arch)?;
    let cmake = find_host_binary_candidates(&["cmake"])?;
    let clang = find_host_binary_candidates(&["clang"])?;
    let pkg_config = find_host_binary_candidates(&["pkg-config"])?;
    let make_program = find_host_binary_candidates(&["make", "gmake"])?;

    write_cross_bin_wrappers(layout, spec, qemu_runner)?;
    write_cmake_toolchain_file(layout, spec, &clang)?;

    let pkgconfig_libdir = format!(
        "{}:{}",
        layout.staging_root.join("usr/lib/pkgconfig").display(),
        layout.staging_root.join("usr/share/pkgconfig").display()
    );
    let command_envs = vec![
        ("PKG_CONFIG_LIBDIR".to_string(), pkgconfig_libdir),
        (
            "PKG_CONFIG_SYSROOT_DIR".to_string(),
            layout.staging_root.display().to_string(),
        ),
        ("PKG_CONFIG_PATH".to_string(), String::new()),
    ];

    Ok(HostCrossBuildEnv {
        cmake,
        pkg_config,
        make_program,
        cmake_toolchain_file: layout.cmake_toolchain_file.clone(),
        command_envs,
    })
}

fn cross_compile_spec(arch: &str) -> anyhow::Result<CrossCompileSpec> {
    match arch {
        "aarch64" => Ok(CrossCompileSpec {
            llvm_target: "aarch64-linux-musl",
            cmake_system_processor: "aarch64",
            guest_tool_dir: "usr/aarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "aarch64-linux-musl",
        }),
        "riscv64" => Ok(CrossCompileSpec {
            llvm_target: "riscv64-linux-musl",
            cmake_system_processor: "riscv64",
            guest_tool_dir: "usr/riscv64-alpine-linux-musl/bin",
            gnu_tool_prefix: "riscv64-linux-musl",
        }),
        "x86_64" => Ok(CrossCompileSpec {
            llvm_target: "x86_64-linux-musl",
            cmake_system_processor: "x86_64",
            guest_tool_dir: "usr/x86_64-alpine-linux-musl/bin",
            gnu_tool_prefix: "x86_64-linux-musl",
        }),
        "loongarch64" => Ok(CrossCompileSpec {
            llvm_target: "loongarch64-linux-musl",
            cmake_system_processor: "loongarch64",
            guest_tool_dir: "usr/loongarch64-alpine-linux-musl/bin",
            gnu_tool_prefix: "loongarch64-linux-musl",
        }),
        _ => bail!(
            "Starry C test cases are only supported on aarch64, riscv64, x86_64, and loongarch64, \
             but got `{arch}`"
        ),
    }
}

fn write_cross_bin_wrappers(
    layout: &CaseAssetLayout,
    spec: CrossCompileSpec,
    qemu_runner: &Path,
) -> anyhow::Result<()> {
    fs::create_dir_all(&layout.cross_bin_dir)
        .with_context(|| format!("failed to create {}", layout.cross_bin_dir.display()))?;
    for tool in CROSS_BINUTILS {
        let guest_relative_path = format!("{}/{tool}", spec.guest_tool_dir);
        ensure_guest_tool_exists(&layout.staging_root, &guest_relative_path)?;
        write_guest_exec_wrapper(
            &layout.cross_bin_dir.join(tool),
            qemu_runner,
            &layout.staging_root,
            &guest_relative_path,
            None,
        )?;
        write_guest_exec_wrapper(
            &layout
                .cross_bin_dir
                .join(format!("{}-{tool}", spec.gnu_tool_prefix)),
            qemu_runner,
            &layout.staging_root,
            &guest_relative_path,
            None,
        )?;
    }

    Ok(())
}

fn write_cmake_toolchain_file(
    layout: &CaseAssetLayout,
    spec: CrossCompileSpec,
    clang: &Path,
) -> anyhow::Result<()> {
    if let Some(parent) = layout.cmake_toolchain_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let sysroot = &layout.staging_root;
    let gcc_toolchain_root = sysroot.join("usr");
    let common_flags = format!(
        "--sysroot={} --gcc-toolchain={} -B{}",
        sysroot.display(),
        gcc_toolchain_root.display(),
        layout.cross_bin_dir.display()
    );

    let mut content = include_str!("cmake-toolchain.cmake.in").to_string();
    for (needle, value) in [
        (
            "@CMAKE_SYSTEM_PROCESSOR@",
            spec.cmake_system_processor.to_string(),
        ),
        ("@CMAKE_SYSROOT@", cmake_value(sysroot)),
        ("@CMAKE_FIND_ROOT_PATH@", cmake_value(sysroot)),
        ("@CMAKE_C_COMPILER@", cmake_value(clang)),
        ("@CMAKE_C_COMPILER_TARGET@", spec.llvm_target.to_string()),
        ("@CMAKE_ASM_COMPILER@", cmake_value(clang)),
        ("@CMAKE_ASM_COMPILER_TARGET@", spec.llvm_target.to_string()),
        ("@CMAKE_AR@", cmake_value(layout.cross_bin_dir.join("ar"))),
        (
            "@CMAKE_RANLIB@",
            cmake_value(layout.cross_bin_dir.join("ranlib")),
        ),
        (
            "@CMAKE_STRIP@",
            cmake_value(layout.cross_bin_dir.join("strip")),
        ),
        (
            "@CMAKE_LINKER@",
            cmake_value(layout.cross_bin_dir.join("ld")),
        ),
        ("@CMAKE_NM@", cmake_value(layout.cross_bin_dir.join("nm"))),
        (
            "@CMAKE_OBJCOPY@",
            cmake_value(layout.cross_bin_dir.join("objcopy")),
        ),
        (
            "@CMAKE_OBJDUMP@",
            cmake_value(layout.cross_bin_dir.join("objdump")),
        ),
        (
            "@CMAKE_READELF@",
            cmake_value(layout.cross_bin_dir.join("readelf")),
        ),
        (
            "@CMAKE_C_COMPILER_AR@",
            cmake_value(layout.cross_bin_dir.join("ar")),
        ),
        (
            "@CMAKE_C_COMPILER_RANLIB@",
            cmake_value(layout.cross_bin_dir.join("ranlib")),
        ),
        ("@CMAKE_C_FLAGS_INIT@", cmake_value(&common_flags)),
        ("@CMAKE_ASM_FLAGS_INIT@", cmake_value(&common_flags)),
        ("@CMAKE_LINKER_FLAGS_INIT@", cmake_value(&common_flags)),
    ] {
        content = content.replace(needle, &value);
    }

    fs::write(&layout.cmake_toolchain_file, content)
        .with_context(|| format!("failed to write {}", layout.cmake_toolchain_file.display()))
}

fn cmake_value(value: impl AsRef<std::ffi::OsStr>) -> String {
    value.as_ref().to_string_lossy().replace('\\', "/")
}

fn build_prebuild_command(
    case: &StarryQemuCase,
    prebuild_script: &Path,
    layout: &CaseAssetLayout,
    prebuild_env: &GuestPrebuildEnv,
) -> anyhow::Result<Command> {
    let guest_busybox = layout.staging_root.join("bin/busybox");
    let guest_shell = layout.staging_root.join("bin/sh");
    let mut command = Command::new(&prebuild_env.qemu_runner);
    command.arg("-L").arg(&layout.staging_root);
    if guest_busybox.is_file() {
        command.arg(&guest_busybox).arg("sh");
    } else {
        ensure!(
            guest_shell.is_file(),
            "staging root is missing guest shell `{}`",
            guest_shell.display()
        );
        command.arg(&guest_shell);
    }
    command
        .arg("-eu")
        .arg(prebuild_script)
        .current_dir(case_c_source_dir(case));
    apply_case_script_envs(&mut command, layout, &prebuild_env.script_envs);
    Ok(command)
}

fn build_cmake_configure_command(
    case: &StarryQemuCase,
    layout: &CaseAssetLayout,
    build_env: &HostCrossBuildEnv,
) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command
        .arg("-S")
        .arg(case_c_source_dir(case))
        .arg("-B")
        .arg(&layout.build_dir)
        .arg("-G")
        .arg("Unix Makefiles")
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_INSTALL_PREFIX=/")
        .arg("-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY")
        .arg(format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            build_env.cmake_toolchain_file.display()
        ))
        .arg(format!(
            "-DCMAKE_MAKE_PROGRAM={}",
            build_env.make_program.display()
        ))
        .arg(format!(
            "-DPKG_CONFIG_EXECUTABLE={}",
            build_env.pkg_config.display()
        ))
        .arg(format!(
            "-D{STARRY_STAGING_ROOT_VAR}={}",
            layout.staging_root.display()
        ));

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn build_cmake_build_command(layout: &CaseAssetLayout, build_env: &HostCrossBuildEnv) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command
        .arg("--build")
        .arg(&layout.build_dir)
        .arg("--parallel");

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn build_cmake_install_command(layout: &CaseAssetLayout, build_env: &HostCrossBuildEnv) -> Command {
    let mut command = Command::new(&build_env.cmake);
    command.arg("--install").arg(&layout.build_dir);
    command.env("DESTDIR", &layout.overlay_dir);

    for (key, value) in &build_env.command_envs {
        command.env(key, value);
    }

    command
}

fn apply_case_script_envs(
    command: &mut Command,
    layout: &CaseAssetLayout,
    script_envs: &[(String, String)],
) {
    let host_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path_entries = Vec::new();
    path_entries.push(layout.command_wrapper_dir.clone());
    path_entries.extend(std::env::split_paths(&host_path));

    command.env("PATH", std::env::join_paths(path_entries).unwrap());
    command.env("QEMU_LD_PREFIX", &layout.staging_root);

    for (key, value) in script_envs {
        command.env(key, value);
    }
}

fn case_script_envs(case: &StarryQemuCase, layout: &CaseAssetLayout) -> Vec<(String, String)> {
    vec![
        (
            STARRY_STAGING_ROOT_VAR.to_string(),
            layout.staging_root.display().to_string(),
        ),
        (
            STARRY_CASE_DIR_VAR.to_string(),
            case.case_dir.display().to_string(),
        ),
        (
            STARRY_CASE_C_DIR_VAR.to_string(),
            case_c_source_dir(case).display().to_string(),
        ),
        (
            STARRY_CASE_WORK_DIR_VAR.to_string(),
            layout.work_dir.display().to_string(),
        ),
        (
            STARRY_CASE_BUILD_DIR_VAR.to_string(),
            layout.build_dir.display().to_string(),
        ),
        (
            STARRY_CASE_OVERLAY_DIR_VAR.to_string(),
            layout.overlay_dir.display().to_string(),
        ),
    ]
}

fn write_guest_command_wrappers(
    layout: &CaseAssetLayout,
    qemu_runner: &Path,
) -> anyhow::Result<()> {
    let mut guest_commands = BTreeMap::new();
    for relative_dir in ["bin", "sbin", "usr/bin", "usr/sbin"] {
        let dir_path = layout.staging_root.join(relative_dir);
        if !dir_path.is_dir() {
            continue;
        }

        let mut entries = fs::read_dir(&dir_path)
            .with_context(|| format!("failed to read {}", dir_path.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read {}", dir_path.display()))?;
        entries.sort_by_key(|left| left.file_name());

        for entry in entries {
            let name = entry.file_name();
            if guest_commands.contains_key(name.as_os_str()) {
                continue;
            }

            let file_type = entry.file_type().with_context(|| {
                format!("failed to inspect guest command {}", entry.path().display())
            })?;
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            guest_commands.insert(
                name,
                format!("{relative_dir}/{}", entry.file_name().to_string_lossy()),
            );
        }
    }

    for (name, relative_guest_path) in guest_commands {
        let wrapper_path = layout.command_wrapper_dir.join(&name);
        if relative_guest_path == "sbin/apk" {
            write_apk_wrapper_script(&wrapper_path, qemu_runner, &layout.staging_root, layout)?;
        } else {
            write_guest_exec_wrapper(
                &wrapper_path,
                qemu_runner,
                &layout.staging_root,
                &relative_guest_path,
                None,
            )?;
        }
    }

    Ok(())
}

fn ensure_guest_tool_exists(staging_root: &Path, relative_path: &str) -> anyhow::Result<()> {
    let path = staging_root.join(relative_path);
    ensure!(
        path.is_file(),
        "staging root is missing required guest tool `{}`; install it in prebuild.sh",
        path.display()
    );
    Ok(())
}

fn qemu_user_binary_names(arch: &str) -> anyhow::Result<&'static [&'static str]> {
    match arch {
        "aarch64" => Ok(&["qemu-aarch64-static", "qemu-aarch64"]),
        "riscv64" => Ok(&["qemu-riscv64-static", "qemu-riscv64"]),
        "x86_64" => Ok(&["qemu-x86_64-static", "qemu-x86_64"]),
        "loongarch64" => Ok(&["qemu-loongarch64-static", "qemu-loongarch64"]),
        _ => bail!(
            "Starry C test cases are only supported on aarch64, riscv64, x86_64, and loongarch64, \
             but got `{arch}`"
        ),
    }
}

fn write_guest_exec_wrapper(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    guest_relative_path: &str,
    extra_args: Option<String>,
) -> anyhow::Result<()> {
    let guest_path = staging_root.join(guest_relative_path);
    let mut body = format!(
        "export QEMU_LD_PREFIX={root}\nexec {qemu} -0 {guest} -L {root} {guest}",
        root = shell_single_quote(staging_root),
        qemu = shell_single_quote(qemu_runner),
        guest = shell_single_quote(&guest_path),
    );
    if let Some(extra_args) = extra_args {
        body.push(' ');
        body.push_str(&extra_args);
    }
    body.push_str(" \"$@\"\n");

    write_wrapper_script(path, &body)
}

fn write_apk_wrapper_script(
    path: &Path,
    qemu_runner: &Path,
    staging_root: &Path,
    layout: &CaseAssetLayout,
) -> anyhow::Result<()> {
    let body = format!(
        "export QEMU_LD_PREFIX={root}\nexec {qemu} -L {root} {apk} --root {root} \
         --repositories-file {repositories} --keys-dir {keys} --cache-dir {cache} --update-cache \
         --timeout 60 --no-interactive --force-no-chroot --scripts=no \"$@\"\n",
        root = shell_single_quote(staging_root),
        qemu = shell_single_quote(qemu_runner),
        apk = shell_single_quote(staging_root.join("sbin/apk")),
        repositories = shell_single_quote(staging_root.join("etc/apk/repositories")),
        keys = shell_single_quote(staging_root.join("etc/apk/keys")),
        cache = shell_single_quote(&layout.apk_cache_dir),
    );
    write_wrapper_script(path, &body)
}

fn write_wrapper_script(path: &Path, body: &str) -> anyhow::Result<()> {
    fs::write(path, format!("#!/bin/sh\nset -eu\n{body}"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).with_context(|| format!("failed to chmod {}", path.display()))
}

fn inject_overlay_tree(rootfs_img: &Path, overlay_dir: &Path) -> anyhow::Result<()> {
    ensure!(
        overlay_has_entries(overlay_dir)?,
        "cmake install did not produce any files under {}",
        overlay_dir.display()
    );

    let mut commands = Vec::new();
    collect_overlay_debugfs_commands(overlay_dir, Path::new(""), &mut commands)?;
    run_debugfs_script(
        rootfs_img,
        &commands,
        &format!(
            "failed to inject overlay {} into {}",
            overlay_dir.display(),
            rootfs_img.display()
        ),
    )
}

fn overlay_has_entries(overlay_dir: &Path) -> anyhow::Result<bool> {
    Ok(fs::read_dir(overlay_dir)
        .with_context(|| format!("failed to read {}", overlay_dir.display()))?
        .next()
        .is_some())
}

fn sync_runtime_dependencies(staging_root: &Path, overlay_dir: &Path) -> anyhow::Result<()> {
    let readelf = find_host_binary_candidates(&["readelf"])?;
    let mut pending = collect_regular_files(overlay_dir)?;
    let mut processed = BTreeSet::new();

    while let Some(path) = pending.pop() {
        if processed.contains(&path) || !is_elf_binary(&path)? {
            continue;
        }
        processed.insert(path.clone());

        let needed = read_needed_shared_libraries(&readelf, &path)?;
        for library in needed {
            let Some(source_path) = find_runtime_library_in_staging_root(staging_root, &library)?
            else {
                continue;
            };
            let relative_path = source_path
                .strip_prefix(staging_root)
                .with_context(|| format!("failed to relativize {}", source_path.display()))?;
            let overlay_path = overlay_dir.join(relative_path);
            if overlay_path.exists() {
                pending.push(overlay_path);
                continue;
            }

            if let Some(parent) = overlay_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }

            let resolved_source =
                fs::canonicalize(&source_path).unwrap_or_else(|_| source_path.clone());
            fs::copy(&resolved_source, &overlay_path).with_context(|| {
                format!(
                    "failed to copy runtime dependency {} to {}",
                    resolved_source.display(),
                    overlay_path.display()
                )
            })?;
            let mode = fs::metadata(&resolved_source)
                .with_context(|| format!("failed to stat {}", resolved_source.display()))?
                .permissions()
                .mode();
            fs::set_permissions(&overlay_path, fs::Permissions::from_mode(mode))
                .with_context(|| format!("failed to chmod {}", overlay_path.display()))?;
            pending.push(overlay_path);
        }
    }

    Ok(())
}

fn collect_regular_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !root.is_dir() {
        return Ok(files);
    }
    for entry in fs::read_dir(root).with_context(|| format!("failed to read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if file_type.is_dir() {
            files.extend(collect_regular_files(&path)?);
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(files)
}

fn read_needed_shared_libraries(readelf: &Path, binary: &Path) -> anyhow::Result<Vec<String>> {
    let output = Command::new(readelf)
        .arg("-d")
        .arg(binary)
        .output()
        .with_context(|| {
            format!(
                "failed to run {} on {}",
                readelf.display(),
                binary.display()
            )
        })?;
    if !output.status.success() {
        bail!(
            "readelf failed for {} with status {}",
            binary.display(),
            output.status
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_needed_shared_library_line)
        .collect())
}

fn parse_needed_shared_library_line(line: &str) -> Option<String> {
    let marker = "Shared library: [";
    let start = line.find(marker)? + marker.len();
    let end = line[start..].find(']')?;
    Some(line[start..start + end].to_string())
}

fn is_elf_binary(path: &Path) -> anyhow::Result<bool> {
    let mut header = [0u8; 4];
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let read = file
        .read(&mut header)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(read == 4 && header == [0x7f, b'E', b'L', b'F'])
}

fn find_runtime_library_in_staging_root(
    staging_root: &Path,
    library: &str,
) -> anyhow::Result<Option<PathBuf>> {
    for relative_dir in RUNTIME_LIBRARY_DIRS {
        let candidate = staging_root.join(relative_dir).join(library);
        if candidate.exists() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn collect_overlay_debugfs_commands(
    overlay_dir: &Path,
    relative_dir: &Path,
    commands: &mut Vec<String>,
) -> anyhow::Result<()> {
    let current_dir = if relative_dir.as_os_str().is_empty() {
        overlay_dir.to_path_buf()
    } else {
        overlay_dir.join(relative_dir)
    };
    let mut entries = fs::read_dir(&current_dir)
        .with_context(|| format!("failed to read {}", current_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read {}", current_dir.display()))?;
    entries.sort_by_key(|left| left.file_name());

    for entry in entries {
        let file_name = PathBuf::from(entry.file_name());
        let relative_path = relative_dir.join(&file_name);
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;

        if file_type.is_dir() {
            commands.push(format!("mkdir /{}", relative_path.display()));
            collect_overlay_debugfs_commands(overlay_dir, &relative_path, commands)?;
            continue;
        }

        ensure!(
            file_type.is_file(),
            "unsupported overlay entry `{}`; only regular files and directories are supported",
            entry.path().display()
        );
        let metadata = fs::metadata(entry.path())
            .with_context(|| format!("failed to stat {}", entry.path().display()))?;
        commands.push(format!(
            "write {} /{}",
            entry.path().display(),
            relative_path.display()
        ));
        commands.push(format!(
            "sif /{} mode 0{:o}",
            relative_path.display(),
            metadata.permissions().mode()
        ));
    }

    Ok(())
}

fn create_usb_backing_image(path: &Path) -> anyhow::Result<()> {
    let file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.set_len(USB_STICK_IMAGE_SIZE)
        .with_context(|| format!("failed to size {}", path.display()))
}

fn run_debugfs_script(
    rootfs_img: &Path,
    commands: &[String],
    context_message: &str,
) -> anyhow::Result<()> {
    eprintln!("debugfs -w {}", rootfs_img.display());
    let mut child = Command::new("debugfs")
        .arg("-w")
        .arg(rootfs_img)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs_img.display()))?;

    {
        let mut stdin = child.stdin.take().context("failed to open debugfs stdin")?;
        for command in commands {
            writeln!(stdin, "{command}").context("failed to write debugfs command")?;
        }
        writeln!(stdin, "quit").context("failed to finalize debugfs script")?;
    }

    let status = child.wait().context("failed to wait for debugfs")?;
    if status.success() {
        Ok(())
    } else {
        bail!("{context_message}: debugfs exited with status {status}");
    }
}

fn find_host_binary_candidates(candidates: &[&str]) -> anyhow::Result<PathBuf> {
    candidates
        .iter()
        .find_map(|candidate| find_optional_host_binary(candidate))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "required host binary was not found in PATH; tried: {}",
                candidates.join(", ")
            )
        })
}

fn find_optional_host_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn shell_single_quote(path: impl AsRef<Path>) -> String {
    let value = path.as_ref().display().to_string().replace('\'', "'\\''");
    format!("'{value}'")
}

async fn decompress_xz_file(input_path: &Path, output_path: &Path) -> anyhow::Result<()> {
    let input_path = input_path.to_path_buf();
    let output_path = output_path.to_path_buf();
    let input_path_for_task = input_path.clone();
    let output_path_for_task = output_path.clone();
    let progress = ProgressBar::new_spinner();
    progress.set_message(format!("decompressing {}", input_path.display()));
    progress.enable_steady_tick(std::time::Duration::from_millis(100));

    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let input = fs::File::open(&input_path_for_task)
            .with_context(|| format!("failed to open {}", input_path_for_task.display()))?;
        let output = fs::File::create(&output_path_for_task)
            .with_context(|| format!("failed to create {}", output_path_for_task.display()))?;

        let mut decoder = XzDecoder::new(input);
        let mut writer = std::io::BufWriter::new(output);
        let mut buffer = vec![0u8; 64 * 1024];

        loop {
            let read = decoder.read(&mut buffer).with_context(|| {
                format!("failed to decompress {}", input_path_for_task.display())
            })?;
            if read == 0 {
                break;
            }
            writer
                .write_all(&buffer[..read])
                .with_context(|| format!("failed to write {}", output_path_for_task.display()))?;
        }
        writer
            .flush()
            .with_context(|| format!("failed to flush {}", output_path_for_task.display()))?;
        Ok(())
    })
    .await
    .context("decompression task failed")??;

    progress.finish_with_message(format!("decompressed {}", output_path.display()));
    tokio_fs::remove_file(&input_path)
        .await
        .with_context(|| format!("failed to remove {}", input_path.display()))?;
    Ok(())
}

async fn run_case_build_script(
    case_dir: &Path,
    workspace_root: &Path,
    arch: &str,
    target: &str,
) -> anyhow::Result<()> {
    let build_script = case_dir.join("build.sh");
    if !build_script.is_file() {
        return Ok(());
    }

    let output = tokio::process::Command::new("bash")
        .arg("./build.sh")
        .current_dir(case_dir)
        .env("STARRY_CASE_DIR", case_dir)
        .env("STARRY_WORKSPACE_ROOT", workspace_root)
        .env("STARRY_ARCH", arch)
        .env("STARRY_TARGET", target)
        .output()
        .await
        .with_context(|| format!("failed to execute {}", build_script.display()))?;
    if output.status.success() {
        return Ok(());
    }

    bail!(
        "case build script {} failed with status {}:\nstdout:\n{}\nstderr:\n{}",
        build_script.display(),
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn inject_rootfs_tree(rootfs_img: &Path, rootfs_files_dir: &Path) -> anyhow::Result<()> {
    inject_rootfs_dir(rootfs_img, rootfs_files_dir, Path::new("/"))
}

fn inject_rootfs_dir(rootfs_img: &Path, host_dir: &Path, image_dir: &Path) -> anyhow::Result<()> {
    ensure_dir_in_rootfs(rootfs_img, image_dir);

    let mut entries = fs::read_dir(host_dir)
        .with_context(|| format!("failed to read {}", host_dir.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to iterate {}", host_dir.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let host_path = entry.path();
        let image_path = image_dir.join(entry.file_name());
        if host_path.is_dir() {
            inject_rootfs_dir(rootfs_img, &host_path, &image_path)?;
            continue;
        }
        if !host_path.is_file() {
            continue;
        }

        if let Some(parent) = image_path.parent() {
            ensure_dir_in_rootfs(rootfs_img, parent);
        }
        debugfs_write(rootfs_img, &host_path, &image_path)?;
    }

    Ok(())
}

fn ensure_dir_in_rootfs(rootfs_img: &Path, image_dir: &Path) {
    let mut current = PathBuf::from("/");
    for component in image_dir.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        current.push(part);
        let _ = Command::new("debugfs")
            .arg("-w")
            .arg("-R")
            .arg(format!("mkdir {}", current.display()))
            .arg(rootfs_img)
            .output();
    }
}

fn debugfs_write(rootfs_img: &Path, host_path: &Path, image_path: &Path) -> anyhow::Result<()> {
    let output = Command::new("debugfs")
        .arg("-w")
        .arg("-R")
        .arg(format!(
            "write {} {}",
            host_path.display(),
            image_path.display()
        ))
        .arg(rootfs_img)
        .output()
        .with_context(|| format!("failed to invoke debugfs for {}", rootfs_img.display()))?;
    if output.status.success() {
        return Ok(());
    }

    bail!(
        "failed to inject {} into {} at {}:\nstdout:\n{}\nstderr:\n{}",
        host_path.display(),
        rootfs_img.display(),
        image_path.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, path::PathBuf, time::Duration};

    use tempfile::tempdir;

    use super::*;

    fn fake_case(root: &Path, name: &str) -> StarryQemuCase {
        let case_dir = root.join("test-suit/starryos/normal").join(name);
        fs::create_dir_all(&case_dir).unwrap();
        StarryQemuCase {
            name: name.to_string(),
            case_dir: case_dir.clone(),
            qemu_config_path: case_dir.join("qemu-aarch64.toml"),
        }
    }

    fn command_env(command: &Command, key: &str) -> Option<String> {
        command.get_envs().find_map(|(name, value)| {
            (name == OsStr::new(key))
                .then(|| value.map(|value| value.to_string_lossy().into_owned()))
                .flatten()
        })
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn resolve_target_dir_uses_workspace_target_directory() {
        let root = tempdir().unwrap();
        let dir = resolve_target_dir(root.path(), "x86_64-unknown-none").unwrap();

        assert_eq!(dir, root.path().join("target/x86_64-unknown-none"));
    }

    #[tokio::test]
    async fn apply_default_qemu_args_includes_rootfs_and_network_defaults() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "x86_64".to_string(),
            target: "x86_64-unknown-none".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let mut qemu = QemuConfig::default();

        apply_default_qemu_args(root.path(), &request, &mut qemu)
            .await
            .unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/x86_64-unknown-none/rootfs-x86_64.img")
                        .display()
                ),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0".to_string(),
            ]
        );
        assert_eq!(
            fs::read(
                root.path()
                    .join("target/x86_64-unknown-none/rootfs-x86_64.img")
            )
            .unwrap(),
            b"rootfs"
        );
    }

    #[tokio::test]
    async fn apply_default_qemu_args_preserves_existing_base_args() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/riscv64gc-unknown-none-elf");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-riscv64.img"), b"rootfs").unwrap();

        let request = ResolvedStarryRequest {
            package: "starryos".to_string(),
            arch: "riscv64".to_string(),
            target: "riscv64gc-unknown-none-elf".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: PathBuf::from("/tmp/.build.toml"),
            build_info_override: None,
            qemu_config: None,
            uboot_config: None,
        };
        let mut qemu = QemuConfig {
            args: vec![
                "-nographic".to_string(),
                "-cpu".to_string(),
                "rv64".to_string(),
                "-machine".to_string(),
                "virt".to_string(),
            ],
            ..Default::default()
        };

        apply_default_qemu_args(root.path(), &request, &mut qemu)
            .await
            .unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-nographic".to_string(),
                "-cpu".to_string(),
                "rv64".to_string(),
                "-machine".to_string(),
                "virt".to_string(),
                "-device".to_string(),
                "virtio-blk-pci,drive=disk0".to_string(),
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/riscv64gc-unknown-none-elf/rootfs-riscv64.img")
                        .display()
                ),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
                "-netdev".to_string(),
                "user,id=net0".to_string(),
            ]
        );
    }

    #[test]
    fn apply_smp_qemu_arg_appends_cpu_count() {
        let mut qemu = QemuConfig {
            args: vec!["-machine".to_string(), "virt".to_string()],
            ..Default::default()
        };

        apply_smp_qemu_arg(&mut qemu, Some(4));

        assert_eq!(
            qemu.args,
            vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "4".to_string(),
            ]
        );
    }

    #[test]
    fn apply_smp_qemu_arg_replaces_existing_cpu_count() {
        let mut qemu = QemuConfig {
            args: vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "1".to_string(),
            ],
            ..Default::default()
        };

        apply_smp_qemu_arg(&mut qemu, Some(4));

        assert_eq!(
            qemu.args,
            vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "4".to_string(),
            ]
        );
    }

    #[test]
    fn smp_from_qemu_arg_reads_plain_cpu_count() {
        let qemu = QemuConfig {
            args: vec![
                "-machine".to_string(),
                "virt".to_string(),
                "-smp".to_string(),
                "4".to_string(),
            ],
            ..Default::default()
        };

        assert_eq!(smp_from_qemu_arg(&qemu), Some(4));
    }

    #[test]
    fn smp_from_qemu_arg_reads_cpus_key_value_syntax() {
        let qemu = QemuConfig {
            args: vec![
                "-machine".to_string(),
                "q35".to_string(),
                "-smp".to_string(),
                "cpus=3,sockets=1,cores=3,threads=1".to_string(),
            ],
            ..Default::default()
        };

        assert_eq!(smp_from_qemu_arg(&qemu), Some(3));
    }

    #[test]
    fn smp_from_qemu_arg_returns_none_when_missing() {
        let qemu = QemuConfig {
            args: vec!["-machine".to_string(), "q35".to_string()],
            ..Default::default()
        };

        assert_eq!(smp_from_qemu_arg(&qemu), None);
    }

    #[tokio::test]
    async fn prepare_case_assets_keeps_default_cases_plain() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();
        let case = fake_case(root.path(), "smoke");

        let assets = prepare_case_assets(root.path(), "x86_64", "x86_64-unknown-none", &case)
            .await
            .unwrap();

        assert_eq!(
            assets.rootfs_path,
            target_dir.join("rootfs-x86_64-smoke.img")
        );
        assert!(assets.extra_qemu_args.is_empty());
        assert_eq!(fs::read(&assets.rootfs_path).unwrap(), b"rootfs");
    }

    #[test]
    fn case_asset_layout_and_usb_qemu_args_use_stable_paths() {
        let root = tempdir().unwrap();
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();

        assert_eq!(
            layout.work_dir,
            root.path()
                .join("target/aarch64-unknown-none-softfloat/starry-cases/usb")
        );
        assert_eq!(
            usb_qemu_args(&layout.usb_stick_path),
            vec![
                "-device".to_string(),
                "qemu-xhci,id=xhci".to_string(),
                "-drive".to_string(),
                format!(
                    "if=none,format=raw,file={},id=usbstick0",
                    layout.usb_stick_path.display()
                ),
                "-device".to_string(),
                "usb-storage,drive=usbstick0,bus=xhci.0".to_string(),
            ]
        );
    }

    #[test]
    fn build_prebuild_command_uses_guest_shell_and_case_envs() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();
        fs::create_dir_all(layout.staging_root.join("bin")).unwrap();
        fs::write(layout.staging_root.join("bin/sh"), b"").unwrap();
        fs::write(layout.staging_root.join("bin/busybox"), b"").unwrap();
        let prebuild_env = GuestPrebuildEnv {
            qemu_runner: PathBuf::from("/usr/bin/qemu-aarch64-static"),
            script_envs: {
                let mut envs = case_script_envs(&case, &layout);
                envs.push((STARRY_APK_REGION_VAR.to_string(), "us".to_string()));
                envs
            },
        };
        let prebuild_script = case_c_source_dir(&case).join("prebuild.sh");

        let command =
            build_prebuild_command(&case, &prebuild_script, &layout, &prebuild_env).unwrap();

        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/usr/bin/qemu-aarch64-static")
        );
        assert_eq!(
            command_args(&command),
            vec![
                "-L".to_string(),
                layout.staging_root.display().to_string(),
                layout
                    .staging_root
                    .join("bin/busybox")
                    .display()
                    .to_string(),
                "sh".to_string(),
                "-eu".to_string(),
                prebuild_script.display().to_string(),
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(case_c_source_dir(&case).as_path())
        );
        assert_eq!(
            command_env(&command, STARRY_CASE_OVERLAY_DIR_VAR),
            Some(layout.overlay_dir.display().to_string())
        );
        assert_eq!(
            command_env(&command, STARRY_APK_REGION_VAR),
            Some("us".to_string())
        );
    }

    #[test]
    fn cmake_configure_command_passes_staging_root_define() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();
        let build_env = HostCrossBuildEnv {
            cmake: PathBuf::from("/usr/bin/cmake"),
            pkg_config: PathBuf::from("/usr/bin/pkg-config"),
            make_program: PathBuf::from("/usr/bin/make"),
            cmake_toolchain_file: PathBuf::from("/tmp/cmake-toolchain.cmake"),
            command_envs: vec![("PKG_CONFIG_LIBDIR".to_string(), "/sysroot".to_string())],
        };

        let command = build_cmake_configure_command(&case, &layout, &build_env);
        let args = command_args(&command);

        assert_eq!(
            command.get_program(),
            std::ffi::OsStr::new("/usr/bin/cmake")
        );
        assert!(args.contains(&format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            build_env.cmake_toolchain_file.display()
        )));
        assert!(args.contains(&format!(
            "-D{STARRY_STAGING_ROOT_VAR}={}",
            layout.staging_root.display()
        )));
        assert_eq!(
            command_env(&command, "PKG_CONFIG_LIBDIR"),
            Some("/sysroot".to_string())
        );
    }

    #[test]
    fn cross_compile_spec_maps_supported_arches() {
        assert_eq!(
            cross_compile_spec("aarch64").unwrap(),
            CrossCompileSpec {
                llvm_target: "aarch64-linux-musl",
                cmake_system_processor: "aarch64",
                guest_tool_dir: "usr/aarch64-alpine-linux-musl/bin",
                gnu_tool_prefix: "aarch64-linux-musl",
            }
        );
        assert_eq!(
            cross_compile_spec("loongarch64").unwrap(),
            CrossCompileSpec {
                llvm_target: "loongarch64-linux-musl",
                cmake_system_processor: "loongarch64",
                guest_tool_dir: "usr/loongarch64-alpine-linux-musl/bin",
                gnu_tool_prefix: "loongarch64-linux-musl",
            }
        );
    }

    #[test]
    fn write_cross_bin_wrappers_generates_prefixed_and_plain_tools() {
        let root = tempdir().unwrap();
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();
        fs::create_dir_all(
            layout
                .staging_root
                .join("usr/aarch64-alpine-linux-musl/bin"),
        )
        .unwrap();
        for tool in [
            "ld", "as", "ar", "ranlib", "strip", "nm", "objcopy", "objdump", "readelf",
        ] {
            let path = layout
                .staging_root
                .join("usr/aarch64-alpine-linux-musl/bin")
                .join(tool);
            fs::write(path, b"").unwrap();
        }

        write_cross_bin_wrappers(
            &layout,
            cross_compile_spec("aarch64").unwrap(),
            Path::new("/usr/bin/qemu-aarch64-static"),
        )
        .unwrap();

        let plain = fs::read_to_string(layout.cross_bin_dir.join("ld")).unwrap();
        let prefixed =
            fs::read_to_string(layout.cross_bin_dir.join("aarch64-linux-musl-ld")).unwrap();
        assert!(plain.contains("qemu-aarch64-static"));
        assert!(plain.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
        assert!(prefixed.contains("usr/aarch64-alpine-linux-musl/bin/ld"));
        assert!(prefixed.contains("-0"));
    }

    #[test]
    fn write_cmake_toolchain_file_contains_clang_cross_settings() {
        let root = tempdir().unwrap();
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();
        fs::create_dir_all(&layout.cross_bin_dir).unwrap();

        write_cmake_toolchain_file(
            &layout,
            cross_compile_spec("aarch64").unwrap(),
            Path::new("/usr/bin/clang"),
        )
        .unwrap();

        let content = fs::read_to_string(&layout.cmake_toolchain_file).unwrap();
        assert!(content.contains("set(CMAKE_SYSTEM_NAME Linux)"));
        assert!(content.contains("set(CMAKE_C_COMPILER \"/usr/bin/clang\")"));
        assert!(content.contains("set(CMAKE_C_COMPILER_TARGET \"aarch64-linux-musl\")"));
        assert!(content.contains("--gcc-toolchain="));
        assert!(content.contains("-B"));
        assert!(content.contains("CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER"));
    }

    #[test]
    fn qemu_user_binary_names_cover_supported_arches() {
        assert_eq!(
            qemu_user_binary_names("aarch64").unwrap(),
            &["qemu-aarch64-static", "qemu-aarch64"]
        );
        assert_eq!(
            qemu_user_binary_names("riscv64").unwrap(),
            &["qemu-riscv64-static", "qemu-riscv64"]
        );
        assert_eq!(
            qemu_user_binary_names("x86_64").unwrap(),
            &["qemu-x86_64-static", "qemu-x86_64"]
        );
        assert_eq!(
            qemu_user_binary_names("loongarch64").unwrap(),
            &["qemu-loongarch64-static", "qemu-loongarch64"]
        );
    }

    #[test]
    fn overlay_debugfs_commands_include_paths_and_modes() {
        let root = tempdir().unwrap();
        let overlay_dir = root.path().join("overlay");
        fs::create_dir_all(overlay_dir.join("usr/bin")).unwrap();
        let binary = overlay_dir.join("usr/bin/test-bin");
        fs::write(&binary, b"bin").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();

        let mut commands = Vec::new();
        collect_overlay_debugfs_commands(&overlay_dir, Path::new(""), &mut commands).unwrap();

        assert_eq!(commands[0], "mkdir /usr");
        assert!(commands.contains(&"mkdir /usr/bin".to_string()));
        assert!(commands.contains(&format!("write {} /usr/bin/test-bin", binary.display())));
        assert!(commands.contains(&"sif /usr/bin/test-bin mode 0100755".to_string()));
    }

    #[test]
    fn preferred_resolver_filters_loopback_and_slirp_addresses() {
        let content = "nameserver 127.0.0.53\nnameserver 10.0.2.3\nnameserver 8.8.8.8\n";
        let usable = content
            .lines()
            .filter_map(parse_nameserver_line)
            .filter(|addr| !addr.is_loopback() && *addr != IpAddr::from([10, 0, 2, 3]))
            .map(|addr| format!("nameserver {addr}"))
            .collect::<Vec<_>>();
        assert_eq!(usable, vec!["nameserver 8.8.8.8".to_string()]);
    }

    #[test]
    fn case_script_envs_include_expected_paths() {
        let root = tempdir().unwrap();
        let case = fake_case(root.path(), "usb");
        let layout =
            case_asset_layout(root.path(), "aarch64-unknown-none-softfloat", "usb").unwrap();

        let envs = case_script_envs(&case, &layout);

        assert!(envs.contains(&(
            STARRY_CASE_DIR_VAR.to_string(),
            case.case_dir.display().to_string()
        )));
        assert!(envs.contains(&(
            STARRY_CASE_BUILD_DIR_VAR.to_string(),
            layout.build_dir.display().to_string()
        )));
    }

    #[test]
    fn parse_apk_region_defaults_to_china() {
        assert_eq!(parse_apk_region(None).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("")).unwrap(), ApkRegion::China);
    }

    #[test]
    fn parse_apk_region_accepts_supported_aliases() {
        assert_eq!(parse_apk_region(Some("china")).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("cn")).unwrap(), ApkRegion::China);
        assert_eq!(parse_apk_region(Some("us")).unwrap(), ApkRegion::Us);
        assert_eq!(parse_apk_region(Some("usa")).unwrap(), ApkRegion::Us);
    }

    #[test]
    fn parse_apk_region_rejects_unknown_value() {
        let err = parse_apk_region(Some("europe")).unwrap_err().to_string();
        assert!(err.contains(STARRY_APK_REGION_VAR));
        assert!(err.contains("china, cn, us, usa"));
    }

    #[test]
    fn rewrite_apk_repositories_switches_to_us_mirror() {
        let input = "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community\n";
        let output = input
            .lines()
            .map(|line| rewrite_apk_repository_line(line, ApkRegion::Us))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            output,
            "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community"
        );
    }

    #[test]
    fn rewrite_apk_repositories_switches_to_china_mirror() {
        let input = "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n";
        let output = input
            .lines()
            .map(|line| rewrite_apk_repository_line(line, ApkRegion::China))
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            output,
            "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community"
        );
    }

    #[test]
    fn rewrite_apk_repositories_for_region_updates_file() {
        let root = tempdir().unwrap();
        let repositories = root.path().join("etc/apk/repositories");
        fs::create_dir_all(repositories.parent().unwrap()).unwrap();
        fs::write(
            &repositories,
            "https://mirrors.cernet.edu.cn/alpine/v3.23/main\nhttps://mirrors.cernet.edu.cn/alpine/v3.23/community\n",
        )
        .unwrap();

        rewrite_apk_repositories_for_region(root.path(), ApkRegion::Us).unwrap();

        assert_eq!(
            fs::read_to_string(&repositories).unwrap(),
            "https://dl-cdn.alpinelinux.org/alpine/v3.23/main\nhttps://dl-cdn.alpinelinux.org/alpine/v3.23/community\n"
        );
    }

    #[test]
    fn format_duration_like_summary_helpers_are_precise_enough() {
        assert_eq!(
            format!("{:.2}", Duration::from_millis(1250).as_secs_f64()),
            "1.25"
        );
    }
}
