use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use ostool::run::qemu::QemuConfig;

use crate::{
    axvisor::{
        context::AxvisorContext,
        image::{config::ImageConfig, spec::ImageSpecRef, storage::Storage},
    },
    context::AxvisorCliArgs,
    test_qemu::AxvisorBoardTestGroup,
};

pub const LINUX_AARCH64_IMAGE_SPEC: &str = "qemu_aarch64_linux";
pub const ARCEOS_RISCV64_IMAGE_SPEC: &str = "qemu_riscv64_arceos";
pub const LINUX_AARCH64_VMCONFIG_TEMPLATE: &str =
    "os/axvisor/configs/vms/linux-aarch64-qemu-smp1.toml";
pub const LINUX_AARCH64_GENERATED_VMCONFIG: &str =
    "os/axvisor/tmp/vmconfigs/linux-aarch64-qemu-smp1.generated.toml";
pub const NIMBOS_X86_64_IMAGE_SPEC: &str = "qemu_x86_64_nimbos";
pub const NIMBOS_X86_64_VMCONFIG: &str = "os/axvisor/configs/vms/nimbos-x86_64-qemu-smp1.toml";
pub const AXVISOR_ROOTFS_IMAGE: &str = "os/axvisor/tmp/rootfs.img";
const RDK_S100_LINUX_GROUP_NAME: &str = "rdk-s100-linux";
const RDK_S100_LINUX_VMCONFIG_TEMPLATE: &str =
    "os/axvisor/configs/vms/linux-aarch64-s100-smp1.toml";
const RDK_S100_LINUX_GENERATED_VMCONFIG: &str =
    "os/axvisor/tmp/vmconfigs/linux-aarch64-s100-ci.generated.toml";
const RDK_S100_LINUX_IMAGE_SPEC: &str = "rdk-s100p_linux";
const RDK_S100_LINUX_KERNEL_IN_IMAGE: &str = "rdk-s100p";

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
    let image_dir = pull_guest_image(ctx, LINUX_AARCH64_IMAGE_SPEC).await?;
    let kernel_path = image_dir.join("qemu-aarch64");
    let rootfs_src = guest_rootfs_path(&image_dir);
    ensure_guest_kernel_exists(&kernel_path, "linux guest")?;
    ensure_guest_rootfs_exists(&rootfs_src, "linux guest")?;

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

pub(crate) async fn prepare_default_rootfs_for_arch(
    ctx: &AxvisorContext,
    arch: &str,
) -> anyhow::Result<PathBuf> {
    let image_spec = match arch {
        "aarch64" => LINUX_AARCH64_IMAGE_SPEC,
        "riscv64" => ARCEOS_RISCV64_IMAGE_SPEC,
        "x86_64" => NIMBOS_X86_64_IMAGE_SPEC,
        _ => return Ok(ctx.workspace_root().join(AXVISOR_ROOTFS_IMAGE)),
    };
    let guest_name = match arch {
        "aarch64" => "linux guest",
        "riscv64" => "riscv64 arceos guest",
        "x86_64" => "nimbos guest",
        _ => unreachable!(),
    };

    let image_dir = pull_guest_image(ctx, image_spec).await?;
    let rootfs_src = guest_rootfs_path(&image_dir);
    ensure_guest_rootfs_exists(&rootfs_src, guest_name)?;

    let rootfs_dst = ctx.workspace_root().join(AXVISOR_ROOTFS_IMAGE);
    copy_rootfs(&rootfs_src, &rootfs_dst)?;
    Ok(rootfs_dst)
}

pub(crate) async fn prepare_nimbos_x86_64_guest_vmconfig(
    ctx: &AxvisorContext,
) -> anyhow::Result<PathBuf> {
    let image_dir = pull_guest_image(ctx, NIMBOS_X86_64_IMAGE_SPEC).await?;
    let kernel_path = image_dir.join("qemu-x86_64");
    let bios_path = image_dir.join("axvm-bios.bin");
    let rootfs_path = guest_rootfs_path(&image_dir);
    ensure_guest_kernel_exists(&kernel_path, "nimbos guest")?;
    if !bios_path.exists() {
        anyhow::bail!("nimbos guest bios not found at {}", bios_path.display());
    }
    ensure_guest_rootfs_exists(&rootfs_path, "nimbos guest")?;

    Ok(ctx.workspace_root().join(NIMBOS_X86_64_VMCONFIG))
}

pub(crate) async fn prepare_board_test_vmconfigs(
    ctx: &AxvisorContext,
    group: &AxvisorBoardTestGroup,
) -> anyhow::Result<Vec<PathBuf>> {
    if group.name != RDK_S100_LINUX_GROUP_NAME {
        return Ok(group.vmconfigs.iter().map(PathBuf::from).collect());
    }

    let image_dir = pull_guest_image(ctx, RDK_S100_LINUX_IMAGE_SPEC).await?;
    let kernel_path = image_dir.join(RDK_S100_LINUX_KERNEL_IN_IMAGE);
    ensure_guest_kernel_exists(&kernel_path, group.name)?;

    let workspace_root = ctx.workspace_root();
    let generated_vmconfig = workspace_root.join(RDK_S100_LINUX_GENERATED_VMCONFIG);
    generate_vmconfig_with_guest_assets(
        &workspace_root.join(RDK_S100_LINUX_VMCONFIG_TEMPLATE),
        &generated_vmconfig,
        &kernel_path,
        None,
        None,
        None,
    )?;

    Ok(vec![generated_vmconfig])
}

pub(crate) fn apply_shell_autoinit_config(config: &mut QemuConfig, shell: &ShellAutoInitConfig) {
    config.success_regex = shell.success_regex.clone();
    config.fail_regex = shell.fail_regex.clone();
    config.shell_prefix = Some(shell.shell_prefix.clone());
    config.shell_init_cmd = Some(shell.shell_init_cmd.clone());
}

fn generate_linux_vmconfig(
    template_path: &Path,
    output_path: &Path,
    kernel_path: &Path,
) -> anyhow::Result<()> {
    generate_vmconfig_with_guest_assets(template_path, output_path, kernel_path, None, None, None)
}

fn generate_vmconfig_with_guest_assets(
    template_path: &Path,
    output_path: &Path,
    kernel_path: &Path,
    dtb_path: Option<&Path>,
    bios_path: Option<&Path>,
    ramdisk_path: Option<&Path>,
) -> anyhow::Result<()> {
    let mut value = read_toml(template_path)?;
    let kernel = value
        .get_mut("kernel")
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| {
            anyhow::anyhow!("missing `[kernel]` section in {}", template_path.display())
        })?;
    kernel.insert(
        "kernel_path".to_string(),
        toml::Value::String(kernel_path.display().to_string()),
    );
    update_optional_guest_path(kernel, "dtb_path", dtb_path);
    update_optional_guest_path(kernel, "bios_path", bios_path);
    update_optional_guest_path(kernel, "ramdisk_path", ramdisk_path);

    write_toml(output_path, &value)
}

fn update_optional_guest_path(
    kernel: &mut toml::map::Map<String, toml::Value>,
    key: &str,
    path: Option<&Path>,
) {
    match path {
        Some(path) => {
            kernel.insert(
                key.to_string(),
                toml::Value::String(path.display().to_string()),
            );
        }
        None => {
            kernel.remove(key);
        }
    }
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

async fn pull_guest_image(ctx: &AxvisorContext, image_spec: &str) -> anyhow::Result<PathBuf> {
    let mut config = ImageConfig::read_config(ctx.workspace_root())?;
    config.local_storage = absolute_path(ctx.workspace_root(), &config.local_storage);

    let storage = Storage::new_from_config(&config).await?;
    storage
        .pull_image(ImageSpecRef::parse(image_spec), None, true)
        .await
}

fn guest_rootfs_path(image_dir: &Path) -> PathBuf {
    image_dir.join("rootfs.img")
}

fn ensure_guest_kernel_exists(kernel_path: &Path, guest_name: &str) -> anyhow::Result<()> {
    if kernel_path.exists() {
        Ok(())
    } else {
        anyhow::bail!("{guest_name} kernel not found at {}", kernel_path.display());
    }
}

fn ensure_guest_rootfs_exists(rootfs_path: &Path, guest_name: &str) -> anyhow::Result<()> {
    if rootfs_path.exists() {
        Ok(())
    } else {
        anyhow::bail!("{guest_name} rootfs not found at {}", rootfs_path.display());
    }
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
        smp: None,
        debug: false,
        vmconfigs: vec![vmconfig],
    }
}

pub(crate) fn uboot_test_build_args(build_config: &str, vmconfig: &str) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(PathBuf::from(build_config)),
        arch: None,
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs: vec![PathBuf::from(vmconfig)],
    }
}

pub(crate) fn board_test_build_args(
    group: &AxvisorBoardTestGroup,
    vmconfigs: Vec<PathBuf>,
) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(PathBuf::from(group.build_config)),
        arch: None,
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs,
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
    fn generate_vmconfig_with_guest_assets_updates_optional_paths() {
        let dir = tempdir().unwrap();
        let template = dir.path().join("linux.toml");
        let output = dir.path().join("out/generated.toml");
        fs::write(
            &template,
            r#"
[base]
id = 2

[kernel]
kernel_path = "old"
dtb_path = "old.dtb"
bios_path = "old.bios"
ramdisk_path = "old.ramdisk"
"#,
        )
        .unwrap();

        generate_vmconfig_with_guest_assets(
            &template,
            &output,
            Path::new("/tmp/kernel.bin"),
            Some(Path::new("/tmp/guest.dtb")),
            None,
            Some(Path::new("/tmp/initrd.img")),
        )
        .unwrap();

        let value: toml::Value = toml::from_str(&fs::read_to_string(&output).unwrap()).unwrap();
        assert_eq!(
            value["kernel"]["kernel_path"].as_str(),
            Some("/tmp/kernel.bin")
        );
        assert_eq!(value["kernel"]["dtb_path"].as_str(), Some("/tmp/guest.dtb"));
        assert!(value["kernel"].get("bios_path").is_none());
        assert_eq!(
            value["kernel"]["ramdisk_path"].as_str(),
            Some("/tmp/initrd.img")
        );
    }

    #[test]
    fn apply_shell_autoinit_config_preserves_existing_args() {
        let mut qemu = QemuConfig {
            args: vec!["-nographic".to_string()],
            ..Default::default()
        };

        apply_shell_autoinit_config(
            &mut qemu,
            &ShellAutoInitConfig {
                shell_prefix: "~ #".to_string(),
                shell_init_cmd: "pwd && echo 'test pass!'".to_string(),
                success_regex: vec!["^test pass!$".to_string()],
                fail_regex: vec!["(?i)panic".to_string()],
            },
        );

        assert_eq!(qemu.args, vec!["-nographic".to_string()]);
        assert_eq!(qemu.shell_prefix.as_deref(), Some("~ #"));
        assert_eq!(
            qemu.shell_init_cmd.as_deref(),
            Some("pwd && echo 'test pass!'")
        );
        assert_eq!(qemu.success_regex, vec!["^test pass!$".to_string()]);
    }

    #[test]
    fn absolute_path_keeps_absolute_paths() {
        let root = Path::new("/workspace");
        let path = Path::new("/tmp/image");

        assert_eq!(absolute_path(root, path), PathBuf::from("/tmp/image"));
    }
}
