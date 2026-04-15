use std::{
    fs,
    io::{self, Read, Write},
    net::IpAddr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, bail, ensure};
use indicatif::ProgressBar;
use ostool::run::qemu::QemuConfig;
use tokio::fs as tokio_fs;
use xz2::read::XzDecoder;

use crate::{
    context::{ResolvedStarryRequest, starry_target_for_arch_checked},
    download::download_to_path_with_progress,
    process::ProcessExt,
};

const ROOTFS_URL: &str = "https://github.com/Starry-OS/rootfs/releases/download/20260214";
const USB_CASE_NAME: &str = "usb";
const USB_GUEST_BINARY_PATH: &str = "/usr/bin/usb-transfer-test";
const USB_GUEST_BINARY_NAME: &str = "usb-transfer-test";
const USB_GUEST_LIBUSB_PATH: &str = "/usr/lib/libusb-1.0.so.0";
const USB_STAGE_LIBUSB_NAME: &str = "libusb-1.0.so.0.5.0";
const USB_WORK_DIR_NAME: &str = "starry-usb";
const USB_STAGING_DIR_NAME: &str = "staging-root";
const USB_BUILD_DIR_NAME: &str = "build";
const USB_TOOLCHAIN_DIR_NAME: &str = "toolchain";
const USB_APK_CACHE_DIR_NAME: &str = "apk-cache";
const USB_STICK_IMAGE_NAME: &str = "usb-stick.raw";
const USB_STICK_IMAGE_SIZE: u64 = 16 * 1024 * 1024;
const HOST_RESOLV_CONF_PATH: &str = "/etc/resolv.conf";
const HOST_RESOLVED_CONF_PATH: &str = "/run/systemd/resolve/resolv.conf";
const DEFAULT_DNS_SERVERS: &[&str] = &["1.1.1.1", "8.8.8.8"];
const USB_APK_PACKAGES: &[&str] = &["build-base", "cmake", "pkgconf", "libusb-dev"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryCaseAssets {
    pub(crate) rootfs_path: PathBuf,
    pub(crate) extra_qemu_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsbCaseLayout {
    work_dir: PathBuf,
    staging_root: PathBuf,
    build_dir: PathBuf,
    toolchain_dir: PathBuf,
    apk_cache_dir: PathBuf,
    host_binary_path: PathBuf,
    usb_stick_path: PathBuf,
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

    // Clean up old per-case copy from a previous run
    if case_rootfs.exists() {
        tokio_fs::remove_file(&case_rootfs).await.with_context(|| {
            format!(
                "failed to remove old per-case rootfs {}",
                case_rootfs.display()
            )
        })?;
    }

    // Copy base rootfs to per-case path
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
    case_name: &str,
) -> anyhow::Result<StarryCaseAssets> {
    let case_rootfs = prepare_per_case_rootfs(workspace_root, arch, target, case_name).await?;

    if uses_usb_case_assets(arch, case_name) {
        let workspace_root = workspace_root.to_path_buf();
        let target = target.to_string();
        let case_rootfs_for_task = case_rootfs.clone();
        let extra_qemu_args = tokio::task::spawn_blocking(move || {
            prepare_usb_case_assets_sync(&workspace_root, &target, &case_rootfs_for_task)
        })
        .await
        .context("usb case asset task failed")??;

        Ok(StarryCaseAssets {
            rootfs_path: case_rootfs,
            extra_qemu_args,
        })
    } else {
        Ok(StarryCaseAssets {
            rootfs_path: case_rootfs,
            extra_qemu_args: Vec::new(),
        })
    }
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

fn uses_usb_case_assets(arch: &str, case_name: &str) -> bool {
    arch == "aarch64" && case_name == USB_CASE_NAME
}

fn usb_case_layout(workspace_root: &Path, target: &str) -> anyhow::Result<UsbCaseLayout> {
    let target_dir = resolve_target_dir(workspace_root, target)?;
    let work_dir = target_dir.join(USB_WORK_DIR_NAME);
    let build_dir = work_dir.join(USB_BUILD_DIR_NAME);
    Ok(UsbCaseLayout {
        staging_root: work_dir.join(USB_STAGING_DIR_NAME),
        toolchain_dir: work_dir.join(USB_TOOLCHAIN_DIR_NAME),
        apk_cache_dir: work_dir.join(USB_APK_CACHE_DIR_NAME),
        host_binary_path: build_dir.join(USB_GUEST_BINARY_NAME),
        usb_stick_path: work_dir.join(USB_STICK_IMAGE_NAME),
        work_dir,
        build_dir,
    })
}

fn usb_case_source_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("test-suit/starryos/normal/usb")
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

fn prepare_usb_case_assets_sync(
    workspace_root: &Path,
    target: &str,
    case_rootfs: &Path,
) -> anyhow::Result<Vec<String>> {
    let layout = usb_case_layout(workspace_root, target)?;
    fs::create_dir_all(&layout.work_dir)
        .with_context(|| format!("failed to create {}", layout.work_dir.display()))?;

    reset_dir(&layout.staging_root)?;
    reset_dir(&layout.build_dir)?;
    fs::create_dir_all(&layout.toolchain_dir)
        .with_context(|| format!("failed to create {}", layout.toolchain_dir.display()))?;
    fs::create_dir_all(&layout.apk_cache_dir)
        .with_context(|| format!("failed to create {}", layout.apk_cache_dir.display()))?;

    populate_staging_root(case_rootfs, &layout.staging_root)?;
    write_host_resolver_config(&layout.staging_root)?;
    install_usb_build_dependencies(&layout.staging_root, &layout.apk_cache_dir)?;
    let build_env = prepare_usb_build_env(&layout)?;
    build_usb_test_binary(workspace_root, &layout, &build_env)?;
    inject_usb_test_assets(case_rootfs, &layout)?;
    create_usb_backing_image(&layout.usb_stick_path)?;

    Ok(usb_qemu_args(&layout.usb_stick_path))
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

fn install_usb_build_dependencies(staging_root: &Path, apk_cache_dir: &Path) -> anyhow::Result<()> {
    let apk_bin = staging_root.join("sbin/apk");
    let repositories_file = staging_root.join("etc/apk/repositories");
    let keys_dir = staging_root.join("etc/apk/keys");

    let output = Command::new("qemu-aarch64-static")
        .arg("-L")
        .arg(staging_root)
        .arg(&apk_bin)
        .arg("--root")
        .arg(staging_root)
        .arg("--repositories-file")
        .arg(&repositories_file)
        .arg("--keys-dir")
        .arg(&keys_dir)
        .arg("--cache-dir")
        .arg(apk_cache_dir)
        .arg("--update-cache")
        .arg("--timeout")
        .arg("60")
        .arg("--no-interactive")
        .arg("--force-no-chroot")
        .arg("add")
        .args(USB_APK_PACKAGES)
        .output()
        .with_context(|| format!("failed to run {}", apk_bin.display()))?;

    io::stdout()
        .write_all(&output.stdout)
        .context("failed to forward apk stdout")?;
    io::stderr()
        .write_all(&output.stderr)
        .context("failed to forward apk stderr")?;

    if !output.status.success() {
        eprintln!(
            "warning: apk exited with status {}; continuing after verifying installed artifacts",
            output.status
        );
    }

    let include_dir = staging_root.join("usr/include/libusb-1.0/libusb.h");
    let pkgconfig_dir = staging_root.join("usr/lib/pkgconfig/libusb-1.0.pc");
    ensure!(
        include_dir.is_file() && pkgconfig_dir.is_file(),
        "usb staging root is missing libusb development files after apk install"
    );

    Ok(())
}

#[derive(Debug, Clone)]
struct UsbBuildEnv {
    cc: PathBuf,
    ar: PathBuf,
    ranlib: PathBuf,
    strip: PathBuf,
    pkg_config: PathBuf,
    configure_envs: Vec<(String, String)>,
}

fn prepare_usb_build_env(layout: &UsbCaseLayout) -> anyhow::Result<UsbBuildEnv> {
    let staging_root = &layout.staging_root;
    let pkg_config = find_host_binary("pkg-config")?;
    let pkgconfig_libdir = format!(
        "{}:{}",
        staging_root.join("usr/lib/pkgconfig").display(),
        staging_root.join("usr/share/pkgconfig").display()
    );

    let gcc_wrapper = layout.toolchain_dir.join("cc");
    let ar_wrapper = layout.toolchain_dir.join("ar");
    let ranlib_wrapper = layout.toolchain_dir.join("ranlib");
    let strip_wrapper = layout.toolchain_dir.join("strip");

    write_wrapper_script(
        &gcc_wrapper,
        &format!(
            "export QEMU_LD_PREFIX={root}\nexec qemu-aarch64-static -L {root} {root}/usr/bin/gcc \
             --sysroot {root} \"$@\"\n",
            root = shell_single_quote(staging_root)
        ),
    )?;
    write_wrapper_script(
        &ar_wrapper,
        &format!(
            "export QEMU_LD_PREFIX={root}\nexec qemu-aarch64-static -L {root} {root}/usr/bin/ar \
             \"$@\"\n",
            root = shell_single_quote(staging_root)
        ),
    )?;
    write_wrapper_script(
        &ranlib_wrapper,
        &format!(
            "export QEMU_LD_PREFIX={root}\nexec qemu-aarch64-static -L {root} \
             {root}/usr/bin/ranlib \"$@\"\n",
            root = shell_single_quote(staging_root)
        ),
    )?;
    write_wrapper_script(
        &strip_wrapper,
        &format!(
            "export QEMU_LD_PREFIX={root}\nexec qemu-aarch64-static -L {root} \
             {root}/usr/bin/strip \"$@\"\n",
            root = shell_single_quote(staging_root)
        ),
    )?;

    Ok(UsbBuildEnv {
        cc: gcc_wrapper,
        ar: ar_wrapper,
        ranlib: ranlib_wrapper,
        strip: strip_wrapper,
        pkg_config,
        configure_envs: vec![
            ("PKG_CONFIG_LIBDIR".to_string(), pkgconfig_libdir),
            (
                "PKG_CONFIG_SYSROOT_DIR".to_string(),
                staging_root.display().to_string(),
            ),
            ("PKG_CONFIG_PATH".to_string(), String::new()),
        ],
    })
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

fn build_usb_test_binary(
    workspace_root: &Path,
    layout: &UsbCaseLayout,
    build_env: &UsbBuildEnv,
) -> anyhow::Result<()> {
    let source_dir = usb_case_source_dir(workspace_root);
    let host_make = find_host_binary("make")?;
    ensure!(
        source_dir.join("CMakeLists.txt").is_file(),
        "missing usb case CMakeLists.txt at {}",
        source_dir.display()
    );

    let mut configure = Command::new("cmake");
    configure
        .arg("-S")
        .arg(&source_dir)
        .arg("-B")
        .arg(&layout.build_dir)
        .arg("-G")
        .arg("Unix Makefiles")
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY")
        .arg(format!("-DCMAKE_SYSROOT={}", layout.staging_root.display()))
        .arg(format!("-DCMAKE_C_COMPILER={}", build_env.cc.display()))
        .arg(format!("-DCMAKE_AR={}", build_env.ar.display()))
        .arg(format!("-DCMAKE_RANLIB={}", build_env.ranlib.display()))
        .arg(format!("-DCMAKE_STRIP={}", build_env.strip.display()))
        .arg(format!("-DCMAKE_MAKE_PROGRAM={}", host_make.display()))
        .arg(format!(
            "-DPKG_CONFIG_EXECUTABLE={}",
            build_env.pkg_config.display()
        ));
    for (key, value) in &build_env.configure_envs {
        configure.env(key, value);
    }
    configure.exec().context("failed to configure usb C test")?;

    let mut build = Command::new("cmake");
    build
        .arg("--build")
        .arg(&layout.build_dir)
        .arg("--parallel");
    for (key, value) in &build_env.configure_envs {
        build.env(key, value);
    }
    build.exec().context("failed to build usb C test")?;

    ensure!(
        layout.host_binary_path.is_file(),
        "usb test binary was not produced at {}",
        layout.host_binary_path.display()
    );

    Ok(())
}

fn inject_usb_test_assets(case_rootfs: &Path, layout: &UsbCaseLayout) -> anyhow::Result<()> {
    let stage_libusb = layout
        .staging_root
        .join(format!("usr/lib/{USB_STAGE_LIBUSB_NAME}"));
    ensure!(
        stage_libusb.is_file(),
        "missing staged libusb runtime at {}",
        stage_libusb.display()
    );

    run_debugfs_script(
        case_rootfs,
        &[
            "cd /usr/bin".to_string(),
            format!(
                "write {} {}",
                layout.host_binary_path.display(),
                USB_GUEST_BINARY_NAME
            ),
            format!("sif {} mode 0100755", USB_GUEST_BINARY_NAME),
        ],
        &format!(
            "failed to inject {} into {}",
            USB_GUEST_BINARY_PATH,
            case_rootfs.display()
        ),
    )?;

    run_debugfs_script(
        case_rootfs,
        &[
            "cd /usr/lib".to_string(),
            format!("write {} libusb-1.0.so.0", stage_libusb.display()),
            "sif libusb-1.0.so.0 mode 0100644".to_string(),
        ],
        &format!(
            "failed to inject {} into {}",
            USB_GUEST_LIBUSB_PATH,
            case_rootfs.display()
        ),
    )
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

fn find_host_binary(name: &str) -> anyhow::Result<PathBuf> {
    find_optional_host_binary(name)
        .ok_or_else(|| anyhow::anyhow!("required host binary `{name}` was not found in PATH"))
}

fn find_optional_host_binary(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

fn shell_single_quote(path: &Path) -> String {
    let value = path.display().to_string().replace('\'', "'\\''");
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
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;

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

    #[tokio::test]
    async fn prepare_case_assets_keeps_default_cases_plain() {
        let root = tempdir().unwrap();
        let target_dir = root.path().join("target/x86_64-unknown-none");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("rootfs-x86_64.img"), b"rootfs").unwrap();

        let assets = prepare_case_assets(root.path(), "x86_64", "x86_64-unknown-none", "smoke")
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
    fn usb_case_layout_and_qemu_args_use_stable_paths() {
        let root = tempdir().unwrap();
        let layout = usb_case_layout(root.path(), "aarch64-unknown-none-softfloat").unwrap();

        assert_eq!(
            layout.work_dir,
            root.path()
                .join("target/aarch64-unknown-none-softfloat/starry-usb")
        );
        assert_eq!(
            layout.host_binary_path,
            root.path()
                .join("target/aarch64-unknown-none-softfloat/starry-usb/build/usb-transfer-test")
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
        assert_eq!(USB_GUEST_BINARY_PATH, "/usr/bin/usb-transfer-test");
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
}
