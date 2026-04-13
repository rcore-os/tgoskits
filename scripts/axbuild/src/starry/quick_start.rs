use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};

#[derive(Args, Debug, Clone)]
pub struct ArgsQuickStart {
    #[command(subcommand)]
    pub command: QuickStartCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum QuickStartCommand {
    /// List supported quick-start platforms and templates
    List,
    #[command(name = "qemu-aarch64")]
    QemuAarch64(ArgsQuickQemu),
    #[command(name = "qemu-riscv64")]
    QemuRiscv64(ArgsQuickQemu),
    #[command(name = "qemu-loongarch64")]
    QemuLoongarch64(ArgsQuickQemu),
    #[command(name = "qemu-x86_64")]
    QemuX8664(ArgsQuickQemu),
    #[command(name = "orangepi-5-plus")]
    Orangepi5Plus(ArgsQuickOrange),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsQuickQemu {
    #[command(subcommand)]
    pub action: QuickQemuAction,
}

#[derive(Subcommand, Debug, Clone, Copy)]
pub enum QuickQemuAction {
    /// Prepare rootfs and build StarryOS for the selected QEMU platform
    Build,
    /// Build and run StarryOS for the selected QEMU platform
    Run,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsQuickOrange {
    #[command(subcommand)]
    pub action: QuickOrangeAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum QuickOrangeAction {
    /// Build StarryOS for Orange Pi 5 Plus
    Build(QuickOrangeConfigArgs),
    /// Build and run StarryOS via U-Boot for Orange Pi 5 Plus
    Run(QuickOrangeRunArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct QuickOrangeConfigArgs {
    /// Override serial device in the tmp U-Boot config
    #[arg(long)]
    pub serial: Option<String>,
    /// Override baud rate in the tmp U-Boot config
    #[arg(long)]
    pub baud: Option<String>,
    /// Override DTB path in the tmp U-Boot config
    #[arg(long)]
    pub dtb: Option<PathBuf>,
}

pub type QuickOrangeRunArgs = QuickOrangeConfigArgs;

#[derive(Debug, Clone, Copy)]
pub enum QuickQemuPlatform {
    Aarch64,
    Riscv64,
    Loongarch64,
    X8664,
}

impl QuickQemuPlatform {
    pub const fn arch(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::Riscv64 => "riscv64",
            Self::Loongarch64 => "loongarch64",
            Self::X8664 => "x86_64",
        }
    }

    pub const fn cli_name(self) -> &'static str {
        match self {
            Self::Aarch64 => "qemu-aarch64",
            Self::Riscv64 => "qemu-riscv64",
            Self::Loongarch64 => "qemu-loongarch64",
            Self::X8664 => "qemu-x86_64",
        }
    }
}

pub fn print_supported_platforms(workspace_root: &Path) {
    println!("Supported platforms:");
    for platform in [
        QuickQemuPlatform::Aarch64,
        QuickQemuPlatform::Riscv64,
        QuickQemuPlatform::Loongarch64,
        QuickQemuPlatform::X8664,
    ] {
        let build = qemu_build_config_path(workspace_root, platform);
        let run = qemu_run_config_path(workspace_root, platform);
        let tmp_build = tmp_qemu_build_config_path(workspace_root, platform);
        let tmp_run = tmp_qemu_run_config_path(workspace_root, platform);
        println!("  {}", platform.cli_name());
        println!("    build template: {}", build.display());
        println!("    run template:   {}", run.display());
        println!("    tmp build cfg:  {}", tmp_build.display());
        println!("    tmp run cfg:    {}", tmp_run.display());
        println!(
            "    commands:\n      cargo xtask starry quick-start {} build\n      cargo xtask \
             starry quick-start {} run",
            platform.cli_name(),
            platform.cli_name()
        );
    }

    let build = orangepi_build_config_path(workspace_root);
    let run = orangepi_uboot_config_path(workspace_root);
    let tmp_build = tmp_orangepi_build_config_path(workspace_root);
    let tmp_run = tmp_orangepi_uboot_config_path(workspace_root);
    println!("  orangepi-5-plus");
    println!("    build template: {}", build.display());
    println!("    run template:   {}", run.display());
    println!("    tmp build cfg:  {}", tmp_build.display());
    println!("    tmp run cfg:    {}", tmp_run.display());
    println!(
        "    commands:\n      cargo xtask starry quick-start orangepi-5-plus build\n      cargo \
         xtask starry quick-start orangepi-5-plus run --serial /dev/ttyUSB0"
    );
}

pub fn qemu_build_config_path(workspace_root: &Path, platform: QuickQemuPlatform) -> PathBuf {
    workspace_root
        .join("os/StarryOS/configs/qemu")
        .join(match platform {
            QuickQemuPlatform::Aarch64 => "build-aarch64.toml",
            QuickQemuPlatform::Riscv64 => "build-riscv64.toml",
            QuickQemuPlatform::Loongarch64 => "build-loongarch64.toml",
            QuickQemuPlatform::X8664 => "build-x86_64.toml",
        })
}

pub fn qemu_run_config_path(workspace_root: &Path, platform: QuickQemuPlatform) -> PathBuf {
    workspace_root
        .join("os/StarryOS/configs/qemu")
        .join(match platform {
            QuickQemuPlatform::Aarch64 => "qemu-aarch64.toml",
            QuickQemuPlatform::Riscv64 => "qemu-riscv64.toml",
            QuickQemuPlatform::Loongarch64 => "qemu-loongarch64.toml",
            QuickQemuPlatform::X8664 => "qemu-x86_64.toml",
        })
}

pub fn orangepi_build_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/configs/board/orangepi-5-plus.toml")
}

pub fn orangepi_uboot_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/configs/board/orangepi-5-plus-uboot.toml")
}

pub fn tmp_config_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/tmp/configs")
}

pub fn tmp_qemu_build_config_path(workspace_root: &Path, platform: QuickQemuPlatform) -> PathBuf {
    tmp_config_dir(workspace_root).join(match platform {
        QuickQemuPlatform::Aarch64 => "build-aarch64.toml",
        QuickQemuPlatform::Riscv64 => "build-riscv64.toml",
        QuickQemuPlatform::Loongarch64 => "build-loongarch64.toml",
        QuickQemuPlatform::X8664 => "build-x86_64.toml",
    })
}

pub fn tmp_qemu_run_config_path(workspace_root: &Path, platform: QuickQemuPlatform) -> PathBuf {
    tmp_config_dir(workspace_root).join(match platform {
        QuickQemuPlatform::Aarch64 => "qemu-aarch64.toml",
        QuickQemuPlatform::Riscv64 => "qemu-riscv64.toml",
        QuickQemuPlatform::Loongarch64 => "qemu-loongarch64.toml",
        QuickQemuPlatform::X8664 => "qemu-x86_64.toml",
    })
}

pub fn tmp_orangepi_build_config_path(workspace_root: &Path) -> PathBuf {
    tmp_config_dir(workspace_root).join("orangepi-5-plus.toml")
}

pub fn tmp_orangepi_uboot_config_path(workspace_root: &Path) -> PathBuf {
    tmp_config_dir(workspace_root).join("orangepi-5-plus-uboot.toml")
}

pub fn default_orangepi_dtb_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/configs/board/orangepi-5-plus.dtb")
}

fn copy_template(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}

pub fn refresh_qemu_configs(
    workspace_root: &Path,
    platform: QuickQemuPlatform,
) -> anyhow::Result<()> {
    copy_template(
        &qemu_build_config_path(workspace_root, platform),
        &tmp_qemu_build_config_path(workspace_root, platform),
    )?;
    copy_template(
        &qemu_run_config_path(workspace_root, platform),
        &tmp_qemu_run_config_path(workspace_root, platform),
    )?;
    Ok(())
}

pub fn ensure_qemu_configs(
    workspace_root: &Path,
    platform: QuickQemuPlatform,
) -> anyhow::Result<()> {
    let build_cfg = tmp_qemu_build_config_path(workspace_root, platform);
    let run_cfg = tmp_qemu_run_config_path(workspace_root, platform);
    if !build_cfg.exists() || !run_cfg.exists() {
        refresh_qemu_configs(workspace_root, platform)?;
    }
    Ok(())
}

pub fn refresh_orangepi_configs(workspace_root: &Path) -> anyhow::Result<()> {
    copy_template(
        &orangepi_build_config_path(workspace_root),
        &tmp_orangepi_build_config_path(workspace_root),
    )?;
    copy_template(
        &orangepi_uboot_config_path(workspace_root),
        &tmp_orangepi_uboot_config_path(workspace_root),
    )?;
    Ok(())
}

pub fn ensure_orangepi_configs(workspace_root: &Path) -> anyhow::Result<()> {
    let build_cfg = tmp_orangepi_build_config_path(workspace_root);
    let run_cfg = tmp_orangepi_uboot_config_path(workspace_root);
    if !build_cfg.exists() || !run_cfg.exists() {
        refresh_orangepi_configs(workspace_root)?;
    }
    Ok(())
}

pub fn prepare_orangepi_uboot_config(
    workspace_root: &Path,
    args: &QuickOrangeConfigArgs,
) -> anyhow::Result<PathBuf> {
    let tmp_path = tmp_orangepi_uboot_config_path(workspace_root);

    let mut value: toml::Value = toml::from_str(
        &fs::read_to_string(&tmp_path)
            .with_context(|| format!("failed to read {}", tmp_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", tmp_path.display()))?;

    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("expected top-level TOML table"))?;
    if let Some(serial) = &args.serial {
        table.insert("serial".into(), toml::Value::String(serial.clone()));
    }
    if let Some(baud) = &args.baud {
        table.insert("baud_rate".into(), toml::Value::String(baud.clone()));
    }

    let dtb_path = args
        .dtb
        .clone()
        .unwrap_or_else(|| default_orangepi_dtb_path(workspace_root));
    if !dtb_path.exists() {
        bail!("DTB path does not exist: {}", dtb_path.display());
    }
    table.insert(
        "dtb_file".into(),
        toml::Value::String(dtb_path.display().to_string()),
    );

    fs::write(&tmp_path, toml::to_string_pretty(&value)?)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    Ok(tmp_path)
}
