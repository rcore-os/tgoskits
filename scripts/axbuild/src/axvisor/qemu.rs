use std::{
    fs,
    path::{Path, PathBuf},
};

use ostool::{build::CargoQemuOverrideArgs, run::qemu::QemuConfig};

use crate::context::{QemuRunConfig, ResolvedAxvisorRequest};

const LEGACY_DEFAULT_ROOTFS: &str = "${workspaceFolder}/tmp/rootfs.img";
const AXVISOR_DEFAULT_ROOTFS: &str = "${workspaceFolder}/os/axvisor/tmp/rootfs.img";

pub(crate) fn default_qemu_config_template_path(axvisor_dir: &Path, arch: &str) -> PathBuf {
    axvisor_dir.join(format!("scripts/ostool/qemu-{arch}.toml"))
}

pub(crate) fn default_qemu_run_config(
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<QemuRunConfig> {
    let default_rootfs = default_rootfs_path(&request.axvisor_dir);
    let default_args = CargoQemuOverrideArgs {
        to_bin: Some(default_qemu_to_bin(&request.arch)?),
        args: Some(default_runtime_qemu_args(
            &request.arch,
            Some(&default_rootfs),
        )),
        ..Default::default()
    };

    let override_args = infer_rootfs_path(&request.vmconfigs)?.and_then(|rootfs_path| {
        (rootfs_path != default_rootfs).then_some(CargoQemuOverrideArgs {
            args: Some(default_runtime_qemu_args(&request.arch, Some(&rootfs_path))),
            ..Default::default()
        })
    });

    Ok(QemuRunConfig {
        qemu_config: None,
        default_args,
        override_args: override_args.unwrap_or_default(),
        ..Default::default()
    })
}

pub(crate) fn qemu_override_args_from_template(
    template_path: &Path,
    request: &ResolvedAxvisorRequest,
) -> anyhow::Result<CargoQemuOverrideArgs> {
    let mut config = load_qemu_config(template_path)?;
    let rootfs_path = infer_rootfs_path(&request.vmconfigs)?
        .unwrap_or_else(|| default_rootfs_path(&request.axvisor_dir));
    replace_rootfs_arg(&mut config.args, &rootfs_path);

    Ok(CargoQemuOverrideArgs {
        args: Some(config.args),
        ..Default::default()
    })
}

fn default_qemu_to_bin(arch: &str) -> anyhow::Result<bool> {
    match arch {
        "aarch64" | "riscv64" | "loongarch64" => Ok(true),
        "x86_64" => Ok(false),
        _ => anyhow::bail!(
            "unsupported Axvisor architecture `{arch}`; expected one of aarch64, x86_64, riscv64, \
             loongarch64"
        ),
    }
}

fn default_runtime_qemu_args(arch: &str, rootfs_path: Option<&Path>) -> Vec<String> {
    let rootfs = rootfs_path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| AXVISOR_DEFAULT_ROOTFS.to_string());

    match arch {
        "aarch64" => vec![
            "-nographic".to_string(),
            "-cpu".to_string(),
            "cortex-a72".to_string(),
            "-machine".to_string(),
            "virt,virtualization=on,gic-version=3".to_string(),
            "-smp".to_string(),
            "4".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-append".to_string(),
            "root=/dev/vda rw init=/init".to_string(),
            "-m".to_string(),
            "8g".to_string(),
        ],
        "riscv64" => vec![
            "-nographic".to_string(),
            "-cpu".to_string(),
            "rv64".to_string(),
            "-machine".to_string(),
            "virt".to_string(),
            "-bios".to_string(),
            "default".to_string(),
            "-smp".to_string(),
            "4".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-append".to_string(),
            "root=/dev/vda rw init=/init".to_string(),
            "-m".to_string(),
            "4g".to_string(),
        ],
        "x86_64" => vec![
            "-nographic".to_string(),
            "-cpu".to_string(),
            "host".to_string(),
            "-machine".to_string(),
            "q35".to_string(),
            "-smp".to_string(),
            "1".to_string(),
            "-accel".to_string(),
            "kvm".to_string(),
            "-device".to_string(),
            "virtio-blk-pci,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-m".to_string(),
            "128M".to_string(),
        ],
        "loongarch64" => vec![
            "-nographic".to_string(),
            "-smp".to_string(),
            "4".to_string(),
            "-device".to_string(),
            "virtio-blk-device,drive=disk0".to_string(),
            "-drive".to_string(),
            format!("id=disk0,if=none,format=raw,file={rootfs}"),
            "-append".to_string(),
            "root=/dev/vda rw init=/init".to_string(),
            "-m".to_string(),
            "4g".to_string(),
        ],
        _ => vec![],
    }
}

fn default_rootfs_path(axvisor_dir: &Path) -> PathBuf {
    axvisor_dir.join("tmp/rootfs.img")
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

fn replace_rootfs_arg(args: &mut Vec<String>, rootfs_path: &Path) {
    let rootfs_path = rootfs_path.display().to_string();

    for arg in args {
        if arg.contains(LEGACY_DEFAULT_ROOTFS) {
            *arg = arg.replace(LEGACY_DEFAULT_ROOTFS, &rootfs_path);
        }
        if arg.contains(AXVISOR_DEFAULT_ROOTFS) {
            *arg = arg.replace(AXVISOR_DEFAULT_ROOTFS, &rootfs_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn request(path: PathBuf, arch: &str, target: &str) -> ResolvedAxvisorRequest {
        ResolvedAxvisorRequest {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("os/axvisor")),
            arch: arch.to_string(),
            target: target.to_string(),
            plat_dyn: None,
            debug: false,
            build_info_path: path,
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
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
    fn default_qemu_run_config_uses_ostool_default_path_resolution() {
        let request = request(
            PathBuf::from("os/axvisor/.build-aarch64-unknown-none-softfloat.toml"),
            "aarch64",
            "aarch64-unknown-none-softfloat",
        );
        let run_config = default_qemu_run_config(&request).unwrap();

        assert!(run_config.qemu_config.is_none());
        assert_eq!(run_config.default_args.to_bin, Some(true));
        assert_eq!(
            run_config.default_args.args,
            Some(default_runtime_qemu_args(
                "aarch64",
                Some(&default_rootfs_path(&request.axvisor_dir))
            ))
        );
        assert!(run_config.override_args.args.is_none());
    }

    #[test]
    fn default_qemu_run_config_overrides_rootfs_when_vmconfig_provides_one() {
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

        let run_config = default_qemu_run_config(&ResolvedAxvisorRequest {
            package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
            axvisor_dir: root.path().join("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            debug: false,
            build_info_path: root.path().join(".build.toml"),
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![vmconfig],
        })
        .unwrap();

        assert_eq!(
            run_config.override_args.args,
            Some(default_runtime_qemu_args("aarch64", Some(&rootfs_path)))
        );
    }

    #[test]
    fn qemu_override_args_from_template_uses_axvisor_tmp_rootfs_by_default() {
        let root = tempdir().unwrap();
        let axvisor_dir = root.path().join("os/axvisor");
        let qemu_config = root.path().join("qemu-aarch64.toml");
        fs::create_dir_all(axvisor_dir.join("tmp")).unwrap();
        fs::write(
            &qemu_config,
            format!(
                r#"
args = ["-drive", "id=disk0,if=none,format=raw,file={AXVISOR_DEFAULT_ROOTFS}"]
success_regex = []
fail_regex = []
to_bin = true
uefi = false
"#
            ),
        )
        .unwrap();

        let overrides = qemu_override_args_from_template(
            &qemu_config,
            &ResolvedAxvisorRequest {
                package: crate::axvisor::build::AXVISOR_PACKAGE.to_string(),
                axvisor_dir: axvisor_dir.clone(),
                arch: "aarch64".to_string(),
                target: "aarch64-unknown-none-softfloat".to_string(),
                plat_dyn: None,
                debug: false,
                build_info_path: axvisor_dir.join(".build.toml"),
                qemu_config: Some(qemu_config.clone()),
                uboot_config: None,
                vmconfigs: vec![],
            },
        )
        .unwrap();

        assert_eq!(
            overrides.args,
            Some(vec![
                "-drive".to_string(),
                format!(
                    "id=disk0,if=none,format=raw,file={}",
                    axvisor_dir.join("tmp/rootfs.img").display()
                )
            ])
        );
    }
}
