use std::{
    fs,
    path::{Path, PathBuf},
};

use ostool::run::qemu::QemuConfig;

use crate::context::ResolvedAxvisorRequest;

pub(crate) fn default_qemu_config_template_path(axvisor_dir: &Path, arch: &str) -> PathBuf {
    axvisor_dir.join(format!("scripts/ostool/qemu-{arch}.toml"))
}

pub(crate) fn apply_rootfs_path(
    config: &mut QemuConfig,
    request: &ResolvedAxvisorRequest,
    workspace_root: &Path,
    explicit_rootfs: Option<&Path>,
) -> anyhow::Result<()> {
    let rootfs_path = if let Some(explicit) = explicit_rootfs {
        explicit.to_path_buf()
    } else {
        infer_rootfs_path(&request.vmconfigs)?
            .unwrap_or_else(|| default_rootfs_path(workspace_root, &request.arch))
    };
    ensure_rootfs_drive_arg(&mut config.args, &rootfs_path);
    Ok(())
}

fn default_rootfs_path(workspace_root: &Path, arch: &str) -> PathBuf {
    if let Some(img) = crate::download::unified_rootfs_image_in_tarball(arch) {
        return crate::download::unified_rootfs_dir(workspace_root).join(img);
    }
    workspace_root.join("os/axvisor/tmp/rootfs.img")
}

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
fn load_qemu_config(path: &Path) -> anyhow::Result<QemuConfig> {
    let content = fs::read_to_string(path).map_err(|e| {
        anyhow!(
            "failed to read QEMU config template {}: {e}",
            path.display()
        )
    })?;
    toml::from_str(&content).map_err(|e| {
        anyhow!(
            "failed to parse QEMU config template {}: {e}",
            path.display()
        )
    })
}

fn ensure_rootfs_drive_arg(args: &mut Vec<String>, rootfs_path: &Path) {
    let rootfs_path = rootfs_path.display().to_string();
    let replacement = format!("id=disk0,if=none,format=raw,file={rootfs_path}");
    let mut replaced = false;

    for arg in args.iter_mut() {
        if arg.starts_with("id=disk0,if=none,format=raw,file=") {
            *arg = replacement.clone();
            replaced = true;
        }
    }

    if replaced {
        return;
    }

    if let Some(device_pos) = args.iter().position(|arg| {
        matches!(
            arg.as_str(),
            "virtio-blk-device,drive=disk0" | "virtio-blk-pci,drive=disk0"
        )
    }) {
        let insert_pos = device_pos + 1;
        args.insert(insert_pos, "-drive".to_string());
        args.insert(insert_pos + 1, replacement);
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

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
    fn apply_rootfs_path_overrides_rootfs_when_vmconfig_provides_one() {
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
        apply_rootfs_path(
            &mut qemu,
            &ResolvedAxvisorRequest {
                package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
                axvisor_dir: root.path().join("os/axvisor"),
                arch: "aarch64".to_string(),
                target: "aarch64-unknown-none-softfloat".to_string(),
                plat_dyn: None,
                smp: None,
                debug: false,
                build_info_path: root.path().join(".build.toml"),
                qemu_config: None,
                uboot_config: None,
                vmconfigs: vec![vmconfig],
            },
            root.path(),
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
    fn apply_rootfs_path_uses_unified_rootfs_by_default() {
        let root = tempdir().unwrap();
        let axvisor_dir = root.path().join("os/axvisor");
        let mut qemu = QemuConfig {
            args: vec!["id=disk0,if=none,format=raw,file=/old/tmp/rootfs.img".to_string()],
            ..Default::default()
        };

        apply_rootfs_path(
            &mut qemu,
            &ResolvedAxvisorRequest {
                package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
                axvisor_dir: axvisor_dir.clone(),
                arch: "aarch64".to_string(),
                target: "aarch64-unknown-none-softfloat".to_string(),
                plat_dyn: None,
                smp: None,
                debug: false,
                build_info_path: axvisor_dir.join(".build.toml"),
                qemu_config: None,
                uboot_config: None,
                vmconfigs: vec![],
            },
            root.path(),
        )
        .unwrap();

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
    fn apply_rootfs_path_inserts_drive_arg_when_template_omits_it() {
        let root = tempdir().unwrap();
        let axvisor_dir = root.path().join("os/axvisor");
        let mut qemu = QemuConfig {
            args: vec![
                "-device".to_string(),
                "virtio-blk-device,drive=disk0".to_string(),
                "-append".to_string(),
                "root=/dev/vda rw init=/init".to_string(),
            ],
            ..Default::default()
        };

        apply_rootfs_path(
            &mut qemu,
            &ResolvedAxvisorRequest {
                package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
                axvisor_dir: axvisor_dir.clone(),
                arch: "aarch64".to_string(),
                target: "aarch64-unknown-none-softfloat".to_string(),
                plat_dyn: None,
                smp: None,
                debug: false,
                build_info_path: axvisor_dir.join(".build.toml"),
                qemu_config: None,
                uboot_config: None,
                vmconfigs: vec![],
            },
            root.path(),
        )
        .unwrap();

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
    fn load_qemu_config_parses_template_file() {
        let root = tempdir().unwrap();
        let qemu_config = root.path().join("qemu-aarch64.toml");
        fs::write(
            &qemu_config,
            r#"
args = ["-nographic"]
success_regex = []
fail_regex = []
to_bin = true
uefi = false
"#,
        )
        .unwrap();

        let qemu = load_qemu_config(&qemu_config).unwrap();

        assert_eq!(qemu.args, vec!["-nographic".to_string()]);
        assert!(qemu.to_bin);
    }
}
