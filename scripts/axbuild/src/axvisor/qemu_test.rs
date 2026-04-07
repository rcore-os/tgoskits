use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use ostool::build::CargoQemuOverrideArgs;

use crate::{
    axvisor::{
        context::AxvisorContext,
        image::{config::ImageConfig, spec::ImageSpecRef, storage::Storage},
        qemu::{default_qemu_config_template_path, qemu_override_args_from_template},
    },
    context::{AxvisorCliArgs, ResolvedAxvisorRequest},
};

pub const LINUX_AARCH64_IMAGE_SPEC: &str = "qemu_aarch64_linux";
pub const LINUX_AARCH64_VMCONFIG_TEMPLATE: &str =
    "os/axvisor/configs/vms/linux-aarch64-qemu-smp1.toml";
pub const LINUX_AARCH64_GENERATED_VMCONFIG: &str =
    "os/axvisor/tmp/vmconfigs/linux-aarch64-qemu-smp1.generated.toml";
pub const NIMBOS_X86_64_IMAGE_SPEC: &str = "qemu_x86_64_nimbos";
pub const NIMBOS_X86_64_VMCONFIG: &str = "os/axvisor/configs/vms/nimbos-x86_64-qemu-smp1.toml";
pub const AXVISOR_ROOTFS_IMAGE: &str = "os/axvisor/tmp/rootfs.img";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedLinuxGuestAssets {
    pub image_dir: PathBuf,
    pub generated_vmconfig: PathBuf,
    pub rootfs_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellAutoInitConfig {
    pub shell_prefix: String,
    pub shell_init_cmd: String,
    pub success_regex: Vec<String>,
    pub fail_regex: Vec<String>,
}

pub(crate) async fn prepare_linux_aarch64_guest_assets(
    ctx: &AxvisorContext,
) -> anyhow::Result<PreparedLinuxGuestAssets> {
    let mut config = ImageConfig::read_config(ctx.workspace_root())?;
    config.local_storage = absolute_path(ctx.workspace_root(), &config.local_storage);

    let storage = Storage::new_from_config(&config).await?;
    let image_dir = storage
        .pull_image(ImageSpecRef::parse(LINUX_AARCH64_IMAGE_SPEC), None, true)
        .await?;

    let kernel_path = image_dir.join("qemu-aarch64");
    let rootfs_src = image_dir.join("rootfs.img");
    if !kernel_path.exists() {
        anyhow::bail!("linux guest kernel not found at {}", kernel_path.display());
    }
    if !rootfs_src.exists() {
        anyhow::bail!("linux guest rootfs not found at {}", rootfs_src.display());
    }

    let workspace_root = ctx.workspace_root();
    let generated_vmconfig = workspace_root.join(LINUX_AARCH64_GENERATED_VMCONFIG);
    generate_linux_vmconfig(
        &workspace_root.join(LINUX_AARCH64_VMCONFIG_TEMPLATE),
        &generated_vmconfig,
        &kernel_path,
    )?;

    let rootfs_path = workspace_root.join(AXVISOR_ROOTFS_IMAGE);
    copy_rootfs(&rootfs_src, &rootfs_path)?;

    Ok(PreparedLinuxGuestAssets {
        image_dir,
        generated_vmconfig,
        rootfs_path,
    })
}

pub(crate) async fn prepare_nimbos_x86_64_guest_vmconfig(
    ctx: &AxvisorContext,
) -> anyhow::Result<PathBuf> {
    let mut config = ImageConfig::read_config(ctx.workspace_root())?;
    config.local_storage = absolute_path(ctx.workspace_root(), &config.local_storage);

    let storage = Storage::new_from_config(&config).await?;
    let image_dir = storage
        .pull_image(ImageSpecRef::parse(NIMBOS_X86_64_IMAGE_SPEC), None, true)
        .await?;

    let kernel_path = image_dir.join("qemu-x86_64");
    let bios_path = image_dir.join("axvm-bios.bin");
    let rootfs_path = image_dir.join("rootfs.img");
    if !kernel_path.exists() {
        anyhow::bail!("nimbos guest kernel not found at {}", kernel_path.display());
    }
    if !bios_path.exists() {
        anyhow::bail!("nimbos guest bios not found at {}", bios_path.display());
    }
    if !rootfs_path.exists() {
        anyhow::bail!("nimbos guest rootfs not found at {}", rootfs_path.display());
    }

    Ok(ctx.workspace_root().join(NIMBOS_X86_64_VMCONFIG))
}

pub(crate) fn shell_autoinit_qemu_override_args(
    request: &ResolvedAxvisorRequest,
    shell: &ShellAutoInitConfig,
) -> anyhow::Result<CargoQemuOverrideArgs> {
    let template_path = default_qemu_config_template_path(&request.axvisor_dir, &request.arch);
    let mut overrides = qemu_override_args_from_template(&template_path, request)?;
    overrides.success_regex = Some(shell.success_regex.clone());
    overrides.fail_regex = Some(shell.fail_regex.clone());
    overrides.shell_prefix = Some(shell.shell_prefix.clone());
    overrides.shell_init_cmd = Some(shell.shell_init_cmd.clone());
    Ok(overrides)
}

fn generate_linux_vmconfig(
    template_path: &Path,
    output_path: &Path,
    kernel_path: &Path,
) -> anyhow::Result<()> {
    let mut value = read_toml(template_path)?;
    value
        .get_mut("kernel")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| {
            anyhow::anyhow!("missing `[kernel]` section in {}", template_path.display())
        })?
        .insert(
            "kernel_path".to_string(),
            toml::Value::String(kernel_path.display().to_string()),
        );

    write_toml(output_path, &value)
}

fn copy_rootfs(rootfs_src: &Path, rootfs_dst: &Path) -> anyhow::Result<()> {
    if let Some(parent) = rootfs_dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(rootfs_src, rootfs_dst).with_context(|| {
        format!(
            "failed to copy rootfs {} to {}",
            rootfs_src.display(),
            rootfs_dst.display()
        )
    })?;
    Ok(())
}

fn read_toml(path: &Path) -> anyhow::Result<toml::Value> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_toml(path: &Path, value: &toml::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::write(path, toml::to_string_pretty(value)?)
        .with_context(|| format!("failed to write {}", path.display()))
}

fn absolute_path(workspace_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

pub(crate) fn qemu_test_build_args(arch: &str, vmconfig: PathBuf) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: None,
        arch: Some(arch.to_string()),
        target: None,
        plat_dyn: None,
        vmconfigs: vec![vmconfig],
    }
}

pub(crate) fn uboot_test_build_args(build_config: &str, vmconfig: &str) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(PathBuf::from(build_config)),
        arch: None,
        target: None,
        plat_dyn: None,
        vmconfigs: vec![PathBuf::from(vmconfig)],
    }
}

pub(crate) fn board_test_build_args(
    group: &crate::test_qemu::AxvisorBoardTestGroup,
) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(PathBuf::from(group.build_config)),
        arch: None,
        target: None,
        plat_dyn: None,
        vmconfigs: group.vmconfigs.iter().map(PathBuf::from).collect(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn generate_linux_vmconfig_rewrites_only_kernel_path() {
        let dir = tempdir().unwrap();
        let template = dir.path().join("linux.toml");
        let output = dir.path().join("out/generated.toml");
        fs::write(
            &template,
            r#"
[base]
id = 1

[kernel]
kernel_path = "old"
entry_point = 1
"#,
        )
        .unwrap();

        generate_linux_vmconfig(&template, &output, Path::new("/tmp/kernel.bin")).unwrap();

        let value: toml::Value = toml::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
        assert_eq!(
            value["kernel"]["kernel_path"].as_str(),
            Some("/tmp/kernel.bin")
        );
        assert_eq!(value["kernel"]["entry_point"].as_integer(), Some(1));
        assert_eq!(value["base"]["id"].as_integer(), Some(1));
    }

    #[test]
    fn copy_rootfs_places_image_at_requested_path() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("rootfs.img");
        let dst = dir.path().join("tmp/rootfs.img");
        fs::write(&src, b"rootfs").unwrap();

        copy_rootfs(&src, &dst).unwrap();

        assert_eq!(fs::read(&dst).unwrap(), b"rootfs");
    }

    #[test]
    fn shell_autoinit_qemu_override_args_preserves_existing_args() {
        let dir = tempdir().unwrap();
        let qemu_config = dir
            .path()
            .join("os/axvisor/scripts/ostool/qemu-aarch64.toml");
        fs::create_dir_all(qemu_config.parent().unwrap()).unwrap();
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

        let overrides = shell_autoinit_qemu_override_args(
            &ResolvedAxvisorRequest {
                package: "axvisor".to_string(),
                axvisor_dir: dir.path().join("os/axvisor"),
                arch: "aarch64".to_string(),
                target: "aarch64-unknown-none-softfloat".to_string(),
                plat_dyn: None,
                build_info_path: dir.path().join(".build.toml"),
                qemu_config: None,
                uboot_config: None,
                vmconfigs: vec![],
            },
            &ShellAutoInitConfig {
                shell_prefix: "~ #".to_string(),
                shell_init_cmd: "pwd && echo 'test pass!'".to_string(),
                success_regex: vec!["^test pass!$".to_string()],
                fail_regex: vec!["(?i)panic".to_string()],
            },
        )
        .unwrap();

        assert_eq!(overrides.args.unwrap(), vec!["-nographic".to_string()]);
        assert_eq!(overrides.shell_prefix.as_deref(), Some("~ #"));
        assert_eq!(
            overrides.shell_init_cmd.as_deref(),
            Some("pwd && echo 'test pass!'")
        );
        assert_eq!(
            overrides.success_regex.unwrap(),
            vec!["^test pass!$".to_string()]
        );
    }

    #[test]
    fn absolute_path_keeps_absolute_paths() {
        let root = Path::new("/workspace");
        let path = Path::new("/tmp/image");

        assert_eq!(absolute_path(root, path), PathBuf::from("/tmp/image"));
    }
}
