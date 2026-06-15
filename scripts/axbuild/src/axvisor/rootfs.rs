//! Axvisor-specific rootfs resolution and preparation helpers.
//!
//! Main responsibilities:
//! - Resolve which rootfs image Axvisor should use for a QEMU run
//! - Distinguish between explicit, managed, and VM-config-derived rootfs paths
//! - Prepare managed rootfs images before launch when Axvisor relies on them
//! - Patch QEMU configs with the selected rootfs using Axvisor-specific rules

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, anyhow};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{Axvisor, build};
use crate::{context::ResolvedAxvisorRequest, rootfs, test::qemu as qemu_test};

pub(super) async fn qemu(axvisor: &mut Axvisor, args: super::ArgsQemu) -> anyhow::Result<()> {
    let mut request = axvisor.prepare_request(
        (&args.build).into(),
        args.qemu_config,
        None,
        crate::context::SnapshotPersistence::Store,
    )?;
    axvisor.app.set_debug_mode(request.debug)?;
    let explicit_rootfs = args
        .rootfs
        .map(|rootfs| {
            crate::image::storage::resolve_explicit_rootfs(
                axvisor.app.workspace_root(),
                &request.arch,
                rootfs,
            )
        })
        .transpose()?;
    ensure_qemu_rootfs_ready(
        &request,
        axvisor.app.workspace_root(),
        explicit_rootfs.as_deref(),
    )
    .await?;
    prepare_loongarch_linux_memory_vmconfigs(
        &mut request,
        axvisor.app.workspace_root(),
        explicit_rootfs.as_deref(),
    )?;
    let cargo = build::load_cargo_config(&request)?;
    let qemu =
        load_patched_qemu_config(axvisor, &request, &cargo, explicit_rootfs.as_deref()).await?;
    axvisor
        .app
        .qemu(cargo, request.build_info_path, Some(qemu))
        .await
}

pub(crate) fn prepare_loongarch_linux_memory_vmconfigs(
    request: &mut ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    if request.arch != "loongarch64" || request.vmconfigs.is_empty() {
        return Ok(());
    }

    let rootfs_path = qemu_rootfs_path(request, workspace_root, explicit_rootfs)?;
    let out_dir = workspace_root.join("tmp/axbuild/axvisor/loongarch64");
    let kernel_path = out_dir.join("linux-qemu");
    let firmware_path = loongarch_uefi_firmware_path(workspace_root);
    let mut prepared_vmconfigs = Vec::with_capacity(request.vmconfigs.len());

    for vmconfig in &request.vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        let value: toml::Value = toml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse vm config {}: {e}", vmconfig.display()))?;
        let guest_kernel = value
            .get("kernel")
            .and_then(|kernel| kernel.get("kernel_path"))
            .and_then(|path| path.as_str());

        if guest_kernel != Some("/guest/linux/linux-qemu") {
            prepared_vmconfigs.push(vmconfig.clone());
            continue;
        }

        ensure_loongarch_busybox_serial_init(&rootfs_path, &out_dir)?;
        rootfs::inject::extract_file(&rootfs_path, "/guest/linux/linux-qemu", &kernel_path)
            .with_context(|| {
                format!(
                    "failed to prepare LoongArch Linux kernel from {}",
                    rootfs_path.display()
                )
            })?;

        let prepared_vmconfig = out_dir.join(
            vmconfig
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("linux-rootfs-smp1.toml")),
        );
        fs::create_dir_all(&out_dir)
            .with_context(|| format!("failed to create {}", out_dir.display()))?;
        let mut patched = content
            .replace("image_location = \"fs\"", "image_location = \"memory\"")
            .replace(
                "kernel_path = \"/guest/linux/linux-qemu\"",
                &format!("kernel_path = \"{}\"", kernel_path.display()),
            );
        if let Some(firmware_path) = &firmware_path {
            patched = replace_toml_string_value(
                &patched,
                "uefi_firmware_path",
                &firmware_path.display().to_string(),
            );
        }
        fs::write(&prepared_vmconfig, patched)
            .with_context(|| format!("failed to write {}", prepared_vmconfig.display()))?;
        prepared_vmconfigs.push(prepared_vmconfig);
    }

    request.vmconfigs = prepared_vmconfigs;
    Ok(())
}

fn loongarch_uefi_firmware_path(workspace_root: &Path) -> Option<PathBuf> {
    [
        PathBuf::from("/tmp/ostool/ovmf/loongarch64/code.fd"),
        workspace_root.join("tmp/ostool/ovmf/loongarch64/code.fd"),
        workspace_root.join("tmp/loongarch-uefi-stage1/assets/qemu-binary/QEMU_EFI.fd"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn replace_toml_string_value(content: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key} = ");
    content
        .lines()
        .map(|line| {
            if line.trim_start().starts_with(&prefix) {
                let indent_len = line.len() - line.trim_start().len();
                format!("{}{}\"{}\"", &line[..indent_len], prefix, value)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn ensure_loongarch_busybox_serial_init(rootfs_path: &Path, out_dir: &Path) -> anyhow::Result<()> {
    let Some(inittab) = rootfs::inject::read_text_file(rootfs_path, "/etc/inittab")? else {
        return Ok(());
    };
    let replacement_content = loongarch_busybox_inittab();
    if inittab == replacement_content || rootfs_file_exists(rootfs_path, "/sbin/openrc")? {
        return Ok(());
    }

    fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;
    let replacement = out_dir.join("inittab.busybox");
    write_text_file(&replacement, replacement_content)?;
    rootfs::inject::replace_file(rootfs_path, "/etc/inittab", &replacement)
        .with_context(|| format!("failed to patch /etc/inittab in {}", rootfs_path.display()))?;
    Ok(())
}

fn loongarch_busybox_inittab() -> &'static str {
    concat!(
        "# /etc/inittab - BusyBox init for LoongArch AxVisor rootfs\n",
        "::sysinit:/bin/mount -t proc proc /proc\n",
        "::sysinit:/bin/mount -t sysfs sysfs /sys\n",
        "::sysinit:/bin/mount -t devtmpfs devtmpfs /dev\n",
        "ttyS0::respawn:/bin/sh\n",
        "::ctrlaltdel:/sbin/reboot\n",
        "::shutdown:/bin/umount -a -r\n",
    )
}

fn rootfs_file_exists(rootfs_path: &Path, guest_path: &str) -> anyhow::Result<bool> {
    let output = Command::new("debugfs")
        .arg("-R")
        .arg(format!("stat {guest_path}"))
        .arg(rootfs_path)
        .output()
        .with_context(|| format!("failed to spawn debugfs for {}", rootfs_path.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout.contains("File not found")
        || stdout.contains("File not found by ext2_lookup")
        || stderr.contains("File not found")
        || stderr.contains("File not found by ext2_lookup")
    {
        return Ok(false);
    }
    if output.status.success() {
        return Ok(true);
    }
    Err(anyhow!(
        "failed to stat {guest_path} in {}: {}",
        rootfs_path.display(),
        stderr.trim()
    ))
}

fn write_text_file(path: &Path, content: &str) -> anyhow::Result<()> {
    let mut file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))
}

pub(super) async fn load_patched_qemu_config(
    axvisor: &mut Axvisor,
    request: &ResolvedAxvisorRequest,
    cargo: &Cargo,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<QemuConfig> {
    let config_path = request.qemu_config.clone().unwrap_or_else(|| {
        super::default_qemu_config_template_path(&request.axvisor_dir, &request.arch)
    });
    let mut qemu = axvisor
        .app
        .read_qemu_config_from_path_for_cargo(cargo, &config_path)
        .await?;
    patch_qemu_rootfs(
        &mut qemu,
        request,
        axvisor.app.workspace_root(),
        explicit_rootfs,
    )?;
    qemu_test::apply_dynamic_platform_qemu_boot(&mut qemu, cargo);
    Ok(qemu)
}

/// Ensures the managed rootfs required by an Axvisor QEMU run is available.
pub(crate) async fn ensure_qemu_rootfs_ready(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = managed_rootfs_path(request, workspace_root, explicit_rootfs)?;
    crate::image::storage::ensure_optional_managed_rootfs(
        workspace_root,
        &request.arch,
        rootfs_path.as_deref(),
    )
    .await
}

/// Patches a QEMU config with the rootfs selected for an Axvisor request.
pub(crate) fn patch_qemu_rootfs(
    config: &mut QemuConfig,
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = qemu_rootfs_path(request, workspace_root, explicit_rootfs)?;
    patch_qemu_rootfs_path(config, &rootfs_path);
    Ok(())
}

/// Resolves the rootfs path selected for an Axvisor QEMU request.
pub(crate) fn qemu_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    if let Some(explicit) = explicit_rootfs {
        return Ok(explicit.to_path_buf());
    }

    infer_rootfs_path(&request.vmconfigs)?
        .map(Ok)
        .unwrap_or_else(|| {
            crate::image::storage::default_rootfs_path(workspace_root, &request.arch)
        })
}

/// Patches a QEMU config with a concrete Axvisor rootfs path.
pub(crate) fn patch_qemu_rootfs_path(config: &mut QemuConfig, rootfs_path: &Path) {
    rootfs::qemu::patch_rootfs(
        config,
        rootfs_path,
        rootfs::qemu::RootfsPatchMode::ReplaceDriveOnly,
    );
}

/// Returns the managed rootfs path Axvisor should prepare, if any.
pub(crate) fn managed_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(explicit_rootfs) = explicit_rootfs {
        return crate::image::storage::resolve_managed_rootfs_path(workspace_root, explicit_rootfs);
    }

    if infer_rootfs_path(&request.vmconfigs)?.is_none() {
        return Ok(Some(crate::image::storage::default_rootfs_path(
            workspace_root,
            &request.arch,
        )?));
    }

    Ok(None)
}

/// Infers a rootfs image path from VM config files by looking next to the
/// configured guest kernel image.
pub(crate) fn infer_rootfs_path(vmconfigs: &[PathBuf]) -> anyhow::Result<Option<PathBuf>> {
    for vmconfig in vmconfigs {
        let content = fs::read_to_string(vmconfig)
            .map_err(|e| anyhow!("failed to read vm config {}: {e}", vmconfig.display()))?;
        let value: toml::Value = toml::from_str(&content)
            .map_err(|e| anyhow!("failed to parse vm config {}: {e}", vmconfig.display()))?;
        let Some(kernel_path) = value
            .get("kernel")
            .and_then(|kernel| kernel.get("kernel_path"))
            .and_then(|path| path.as_str())
        else {
            continue;
        };
        let rootfs_path = Path::new(kernel_path)
            .parent()
            .map(|dir| dir.join("rootfs.img"));
        if let Some(rootfs_path) = rootfs_path
            && rootfs_path.exists()
        {
            return Ok(Some(rootfs_path));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn managed_rootfs_path_for_test(root: &Path, image_name: &str) -> PathBuf {
        root.join(".tgos-images").join(image_name).join(image_name)
    }

    fn write_test_image_config(root: &Path) {
        let config = crate::image::config::ImageConfig {
            local_storage: root.join(".tgos-images"),
            registry: crate::image::config::DEFAULT_REGISTRY_URL.to_string(),
            auto_sync: true,
            auto_sync_threshold: 60,
        };
        crate::image::config::ImageConfig::write_config(root, &config).unwrap();
    }

    fn request(root: &Path, vmconfigs: Vec<PathBuf>) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            smp: None,
            debug: false,
            build_info_path: root.join(".build.toml"),
            qemu_config: None,
            uboot_config: None,
            vmconfigs,
        }
    }

    #[test]
    fn infer_rootfs_path_uses_vmconfig_kernel_sibling() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("rootfs.img"), b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        assert_eq!(
            infer_rootfs_path(&[vmconfig]).unwrap(),
            Some(image_dir.join("rootfs.img"))
        );
    }

    #[test]
    fn patch_qemu_rootfs_overrides_rootfs_when_vmconfig_provides_one() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        let rootfs_path = image_dir.join("rootfs.img");
        fs::write(&rootfs_path, b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        let mut qemu = QemuConfig {
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
            ..Default::default()
        };
        patch_qemu_rootfs(
            &mut qemu,
            &request(root.path(), vec![vmconfig]),
            root.path(),
            None,
        )
        .unwrap();

        assert_eq!(
            qemu.args,
            vec![format!(
                "id=disk0,if=none,format=raw,file={}",
                rootfs_path.display()
            )]
        );
    }

    #[test]
    fn patch_qemu_rootfs_uses_unified_rootfs_by_default() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let rootfs = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-alpine.img");
        let mut qemu = QemuConfig {
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![format!(
                "id=disk0,if=none,format=raw,file={}",
                rootfs.display()
            )]
        );
    }

    #[test]
    fn patch_qemu_rootfs_inserts_drive_arg_when_template_omits_it() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let rootfs = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-alpine.img");
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-append".to_string(),
                "root=/dev/vda rw init=/bin/sh".to_string(),
            ],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-drive".to_string(),
                format!("id=disk0,if=none,format=raw,file={}", rootfs.display()),
                "-append".to_string(),
                "root=/dev/vda rw init=/bin/sh".to_string(),
            ]
        );
    }

    #[test]
    fn managed_rootfs_path_uses_default_unified_rootfs_when_vmconfig_has_no_rootfs() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            r#"
[kernel]
kernel_path = "/tmp/qemu-aarch64"
"#,
        )
        .unwrap();

        assert_eq!(
            managed_rootfs_path(&request(root.path(), vec![vmconfig]), root.path(), None).unwrap(),
            Some(managed_rootfs_path_for_test(
                root.path(),
                "rootfs-aarch64-alpine.img"
            ))
        );
    }

    #[test]
    fn managed_rootfs_path_skips_when_vmconfig_provides_kernel_sibling_rootfs() {
        let root = tempdir().unwrap();
        let image_dir = root.path().join("image");
        fs::create_dir_all(&image_dir).unwrap();
        fs::write(image_dir.join("rootfs.img"), b"rootfs").unwrap();
        let vmconfig = root.path().join("vm.toml");
        fs::write(
            &vmconfig,
            format!(
                r#"
[kernel]
kernel_path = "{}"
"#,
                image_dir.join("qemu-aarch64").display()
            ),
        )
        .unwrap();

        assert_eq!(
            managed_rootfs_path(&request(root.path(), vec![vmconfig]), root.path(), None).unwrap(),
            None
        );
    }

    #[test]
    fn managed_rootfs_path_keeps_explicit_managed_rootfs() {
        let root = tempdir().unwrap();
        write_test_image_config(root.path());
        let explicit = managed_rootfs_path_for_test(root.path(), "rootfs-aarch64-debian.img");

        assert_eq!(
            managed_rootfs_path(
                &request(root.path(), vec![]),
                root.path(),
                Some(explicit.as_path())
            )
            .unwrap(),
            Some(explicit)
        );
    }
}
