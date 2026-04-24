//! AxVisor-specific rootfs resolution and preparation helpers.
//!
//! Main responsibilities:
//! - Resolve which rootfs image AxVisor should use for a QEMU run
//! - Distinguish between explicit, managed, and VM-config-derived rootfs paths
//! - Prepare managed rootfs images before launch when AxVisor relies on them
//! - Patch QEMU configs with the selected rootfs using AxVisor-specific rules

use std::{
    fs,
    path::{Path, PathBuf},
};

use ostool::run::qemu::QemuConfig;

use crate::{context::ResolvedAxvisorRequest, rootfs};

/// Resolves an explicit AxVisor rootfs CLI value into a concrete path.
pub(crate) fn resolve_explicit_rootfs(
    workspace_root: &Path,
    arch: &str,
    rootfs: PathBuf,
) -> PathBuf {
    rootfs::store::resolve_rootfs_path(workspace_root, arch, rootfs)
}

/// Ensures the managed rootfs required by an AxVisor QEMU run is available.
pub(crate) async fn ensure_qemu_rootfs_ready(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let Some(rootfs_path) = managed_rootfs_path(request, workspace_root, explicit_rootfs)? else {
        return Ok(());
    };

    rootfs::store::ensure_managed_rootfs(workspace_root, &request.arch, &rootfs_path).await
}

/// Patches a QEMU config with the rootfs selected for an AxVisor request.
pub(crate) fn patch_qemu_rootfs(
    config: &mut QemuConfig,
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = if let Some(explicit) = explicit_rootfs {
        explicit.to_path_buf()
    } else {
        infer_rootfs_path(&request.vmconfigs)?
            .unwrap_or(default_rootfs_path(workspace_root, &request.arch)?)
    };
    rootfs::qemu::patch_rootfs(
        config,
        &rootfs_path,
        rootfs::qemu::RootfsPatchMode::ReplaceDriveOnly,
    );
    Ok(())
}

/// Returns the managed rootfs path AxVisor should prepare, if any.
pub(crate) fn managed_rootfs_path(
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(explicit_rootfs) = explicit_rootfs {
        if explicit_rootfs.starts_with(rootfs::store::rootfs_dir(workspace_root)) {
            return Ok(Some(explicit_rootfs.to_path_buf()));
        }
        return Ok(None);
    }

    if infer_rootfs_path(&request.vmconfigs)?.is_none() {
        return Ok(Some(default_rootfs_path(workspace_root, &request.arch)?));
    }

    Ok(None)
}

/// Returns AxVisor's default managed rootfs path for an architecture.
fn default_rootfs_path(workspace_root: &Path, arch: &str) -> anyhow::Result<PathBuf> {
    let image_name = rootfs::store::default_rootfs_image(arch)
        .ok_or_else(|| anyhow!("no managed rootfs image available for axvisor arch `{arch}`"))?;
    Ok(rootfs::store::rootfs_dir(workspace_root).join(image_name))
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
        let mut qemu = QemuConfig {
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
            ..Default::default()
        };

        patch_qemu_rootfs(&mut qemu, &request(root.path(), vec![]), root.path(), None).unwrap();

        assert_eq!(
            qemu.args,
            vec![format!(
                "id=disk0,if=none,format=raw,file={}",
                root.path()
                    .join("target/rootfs/rootfs-aarch64-alpine.img")
                    .display()
            )]
        );
    }

    #[test]
    fn patch_qemu_rootfs_inserts_drive_arg_when_template_omits_it() {
        let root = tempdir().unwrap();
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-append".to_string(),
                "root=/dev/vda rw init=/init".to_string(),
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
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    root.path()
                        .join("target/rootfs/rootfs-aarch64-alpine.img")
                        .display()
                ),
                "-append".to_string(),
                "root=/dev/vda rw init=/init".to_string(),
            ]
        );
    }

    #[test]
    fn managed_rootfs_path_uses_default_unified_rootfs_when_vmconfig_has_no_rootfs() {
        let root = tempdir().unwrap();
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
            Some(root.path().join("target/rootfs/rootfs-aarch64-alpine.img"))
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
        let explicit = root.path().join("target/rootfs/rootfs-aarch64-debian.img");

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
