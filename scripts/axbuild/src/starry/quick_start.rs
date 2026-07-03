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
    #[command(name = "licheerv-nano-sg2002")]
    LicheervNanoSg2002(ArgsQuickSg2002),
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

impl QuickOrangeConfigArgs {
    fn has_overrides(&self) -> bool {
        self.serial.is_some() || self.baud.is_some() || self.dtb.is_some()
    }
}

#[derive(Args, Debug, Clone)]
pub struct ArgsQuickSg2002 {
    #[command(subcommand)]
    pub action: QuickSg2002Action,
}

#[derive(Subcommand, Debug, Clone)]
pub enum QuickSg2002Action {
    /// Build StarryOS for SG2002
    Build,
    /// Build and run StarryOS on a local SG2002 serial console
    Run(QuickSg2002RunArgs),
}

#[derive(Args, Debug, Clone, Default)]
pub struct QuickSg2002RunArgs {
    /// Override serial device in the tmp SG2002 U-Boot config
    #[arg(long)]
    pub serial: Option<String>,
    /// Override baud rate in the tmp SG2002 U-Boot config
    #[arg(long)]
    pub baud: Option<String>,
}

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
        let tmp_build = tmp_qemu_build_config_path(workspace_root, platform);
        println!("  {}", platform.cli_name());
        println!("    build template: {}", build.display());
        println!(
            "    run template:   {}",
            super::default_qemu_config_template_path(workspace_root, platform.arch()).display()
        );
        println!("    tmp build cfg:  {}", tmp_build.display());
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

    let build = sg2002_build_config_path(workspace_root);
    let board = sg2002_board_config_path(workspace_root);
    let tmp_build = tmp_sg2002_build_config_path(workspace_root);
    println!("  licheerv-nano-sg2002");
    println!("    build template: {}", build.display());
    println!("    board template: {}", board.display());
    println!(
        "    local run template: {}",
        sg2002_uboot_config_path(workspace_root).display()
    );
    println!("    tmp build cfg:  {}", tmp_build.display());
    println!(
        "    commands:\n      cargo xtask starry quick-start licheerv-nano-sg2002 build\n      \
         cargo xtask starry quick-start licheerv-nano-sg2002 run --serial /dev/ttyUSB0"
    );
}

pub fn qemu_build_config_path(workspace_root: &Path, platform: QuickQemuPlatform) -> PathBuf {
    workspace_root
        .join("os/StarryOS/configs/board")
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

pub fn sg2002_build_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/configs/board/licheerv-nano-sg2002.toml")
}

pub fn sg2002_board_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/configs/board/licheerv-nano-sg2002-board.toml")
}

pub fn sg2002_uboot_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("os/StarryOS/configs/board/licheerv-nano-sg2002-uboot.toml")
}

pub fn tmp_config_dir(workspace_root: &Path) -> PathBuf {
    crate::context::axbuild_tmp_dir(workspace_root)
        .join("config")
        .join("starryos")
        .join("quick-start")
}

pub fn tmp_qemu_build_config_path(workspace_root: &Path, platform: QuickQemuPlatform) -> PathBuf {
    tmp_config_dir(workspace_root).join(match platform {
        QuickQemuPlatform::Aarch64 => "build-aarch64.toml",
        QuickQemuPlatform::Riscv64 => "build-riscv64.toml",
        QuickQemuPlatform::Loongarch64 => "build-loongarch64.toml",
        QuickQemuPlatform::X8664 => "build-x86_64.toml",
    })
}

pub fn tmp_orangepi_build_config_path(workspace_root: &Path) -> PathBuf {
    tmp_config_dir(workspace_root).join("orangepi-5-plus.toml")
}

pub fn tmp_orangepi_uboot_config_path(workspace_root: &Path) -> PathBuf {
    tmp_config_dir(workspace_root).join("orangepi-5-plus-uboot.toml")
}

pub fn tmp_sg2002_build_config_path(workspace_root: &Path) -> PathBuf {
    tmp_config_dir(workspace_root).join("licheerv-nano-sg2002.toml")
}

pub fn tmp_sg2002_uboot_config_path(workspace_root: &Path) -> PathBuf {
    tmp_config_dir(workspace_root).join("licheerv-nano-sg2002-uboot.toml")
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

fn copy_build_template(src: &Path, dst: &Path) -> anyhow::Result<()> {
    copy_template(src, dst)?;
    copy_companion_its(src, dst)
}

fn copy_companion_its(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let src_its = src.with_extension("its");
    if !src_its.exists() {
        return Ok(());
    }
    let dst_its = dst.with_extension("its");
    fs::copy(&src_its, &dst_its).with_context(|| {
        format!(
            "failed to copy {} to {}",
            src_its.display(),
            dst_its.display()
        )
    })?;
    Ok(())
}

pub fn refresh_qemu_build_config(
    workspace_root: &Path,
    platform: QuickQemuPlatform,
) -> anyhow::Result<()> {
    copy_build_template(
        &qemu_build_config_path(workspace_root, platform),
        &tmp_qemu_build_config_path(workspace_root, platform),
    )?;
    Ok(())
}

pub fn ensure_qemu_build_config(
    workspace_root: &Path,
    platform: QuickQemuPlatform,
) -> anyhow::Result<()> {
    let build_cfg = tmp_qemu_build_config_path(workspace_root, platform);
    if !build_cfg.exists() {
        refresh_qemu_build_config(workspace_root, platform)?;
    }
    Ok(())
}

pub fn refresh_orangepi_configs(workspace_root: &Path) -> anyhow::Result<()> {
    copy_build_template(
        &orangepi_build_config_path(workspace_root),
        &tmp_orangepi_build_config_path(workspace_root),
    )?;
    copy_template(
        &orangepi_uboot_config_path(workspace_root),
        &tmp_orangepi_uboot_config_path(workspace_root),
    )?;
    Ok(())
}

pub fn refresh_sg2002_config(workspace_root: &Path) -> anyhow::Result<()> {
    copy_build_template(
        &sg2002_build_config_path(workspace_root),
        &tmp_sg2002_build_config_path(workspace_root),
    )?;
    copy_template(
        &sg2002_uboot_config_path(workspace_root),
        &tmp_sg2002_uboot_config_path(workspace_root),
    )?;
    Ok(())
}

pub fn ensure_sg2002_config(workspace_root: &Path) -> anyhow::Result<()> {
    let build_cfg = tmp_sg2002_build_config_path(workspace_root);
    if !build_cfg.exists() {
        copy_build_template(&sg2002_build_config_path(workspace_root), &build_cfg)?;
    }
    let run_cfg = tmp_sg2002_uboot_config_path(workspace_root);
    if !run_cfg.exists() {
        copy_template(&sg2002_uboot_config_path(workspace_root), &run_cfg)?;
    }
    Ok(())
}

pub fn ensure_orangepi_configs(workspace_root: &Path) -> anyhow::Result<()> {
    let build_cfg = tmp_orangepi_build_config_path(workspace_root);
    let run_cfg = tmp_orangepi_uboot_config_path(workspace_root);
    if !build_cfg.exists() {
        copy_build_template(&orangepi_build_config_path(workspace_root), &build_cfg)?;
    }
    if !run_cfg.exists() {
        copy_template(&orangepi_uboot_config_path(workspace_root), &run_cfg)?;
    }
    Ok(())
}

pub fn prepare_orangepi_uboot_config(
    workspace_root: &Path,
    args: &QuickOrangeConfigArgs,
) -> anyhow::Result<PathBuf> {
    let tmp_path = tmp_orangepi_uboot_config_path(workspace_root);
    let tmp_exists = tmp_path.exists();
    if tmp_exists && !args.has_overrides() {
        return Ok(tmp_path);
    }

    if !tmp_exists {
        copy_template(&orangepi_uboot_config_path(workspace_root), &tmp_path)?;
    }

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

    if let Some(dtb_path) = args
        .dtb
        .clone()
        .or_else(|| (!tmp_exists).then(|| default_orangepi_dtb_path(workspace_root)))
    {
        if !dtb_path.exists() {
            bail!("DTB path does not exist: {}", dtb_path.display());
        }
        table.insert(
            "dtb_file".into(),
            toml::Value::String(dtb_path.display().to_string()),
        );
    }

    fs::write(&tmp_path, toml::to_string_pretty(&value)?)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    Ok(tmp_path)
}

pub fn prepare_sg2002_uboot_config(
    workspace_root: &Path,
    args: &QuickSg2002RunArgs,
) -> anyhow::Result<PathBuf> {
    let tmp_path = tmp_sg2002_uboot_config_path(workspace_root);
    let tmp_exists = tmp_path.exists();

    if !tmp_exists {
        copy_template(&sg2002_uboot_config_path(workspace_root), &tmp_path)?;
    }

    let template: toml::Value = toml::from_str(
        &fs::read_to_string(sg2002_uboot_config_path(workspace_root)).with_context(|| {
            format!(
                "failed to read {}",
                sg2002_uboot_config_path(workspace_root).display()
            )
        })?,
    )
    .with_context(|| {
        format!(
            "failed to parse {}",
            sg2002_uboot_config_path(workspace_root).display()
        )
    })?;
    let mut value: toml::Value = toml::from_str(
        &fs::read_to_string(&tmp_path)
            .with_context(|| format!("failed to read {}", tmp_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", tmp_path.display()))?;

    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("expected top-level TOML table"))?;
    let template_table = template
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("expected top-level TOML table"))?;
    for key in [
        "kernel_load_addr",
        "fit_load_addr",
        "dtb_file",
        "shell_prefix",
        "shell_init_cmd",
        "success_regex",
        "fail_regex",
        "timeout",
    ] {
        if !table.contains_key(key)
            && let Some(value) = template_table.get(key)
        {
            table.insert(key.to_string(), value.clone());
        }
    }
    if let Some(serial) = &args.serial {
        table.insert("serial".into(), toml::Value::String(serial.clone()));
    }
    if let Some(baud) = &args.baud {
        table.insert("baud_rate".into(), toml::Value::String(baud.clone()));
    }

    fs::write(&tmp_path, toml::to_string_pretty(&value)?)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    Ok(tmp_path)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;

    fn write_orangepi_uboot_template(root: &Path) {
        let path = orangepi_uboot_config_path(root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            "serial = \"/dev/template\"\nbaud_rate = \"1500000\"\ndtb_file = \"template.dtb\"\n",
        )
        .unwrap();
    }

    fn write_sg2002_uboot_template(root: &Path) {
        let path = sg2002_uboot_config_path(root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            path,
            "serial = \"/dev/template\"\nbaud_rate = \"115200\"\nkernel_load_addr = \
             \"0x80200000\"\nfit_load_addr = \"0x82200000\"\ndtb_file = \"sg2002.dtb\"\n",
        )
        .unwrap();
    }

    fn write_sg2002_build_template(root: &Path) {
        let path = sg2002_build_config_path(root);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            "target = \"riscv64gc-unknown-none-elf\"\nfeatures = [\n  \
             \"starry-kernel/sg2002\",\n  \"axplat-dyn/thead-mae\",\n  \"ax-driver/cvsd\",\n  \
             \"ax-driver/serial\",\n]\nlog = \"Info\"\n",
        )
        .unwrap();
        fs::write(path.with_extension("its"), "ITS_TEMPLATE").unwrap();
    }

    #[test]
    fn prepare_orangepi_uboot_config_keeps_existing_tmp_without_overrides() {
        let root = tempdir().unwrap();
        write_orangepi_uboot_template(root.path());
        let tmp_path = tmp_orangepi_uboot_config_path(root.path());
        fs::create_dir_all(tmp_path.parent().unwrap()).unwrap();
        fs::write(
            &tmp_path,
            "serial = \"/dev/existing\"\nbaud_rate = \"9600\"\ndtb_file = \"existing.dtb\"\n",
        )
        .unwrap();

        prepare_orangepi_uboot_config(root.path(), &QuickOrangeConfigArgs::default()).unwrap();

        let value: toml::Value = toml::from_str(&fs::read_to_string(tmp_path).unwrap()).unwrap();
        assert_eq!(value["serial"].as_str(), Some("/dev/existing"));
        assert_eq!(value["baud_rate"].as_str(), Some("9600"));
        assert_eq!(value["dtb_file"].as_str(), Some("existing.dtb"));
    }

    #[test]
    fn prepare_orangepi_uboot_config_updates_existing_tmp_overrides() {
        let root = tempdir().unwrap();
        write_orangepi_uboot_template(root.path());
        let tmp_path = tmp_orangepi_uboot_config_path(root.path());
        fs::create_dir_all(tmp_path.parent().unwrap()).unwrap();
        fs::write(
            &tmp_path,
            "serial = \"/dev/existing\"\nbaud_rate = \"9600\"\ndtb_file = \"existing.dtb\"\n",
        )
        .unwrap();
        let dtb_path = root.path().join("custom.dtb");
        fs::write(&dtb_path, "").unwrap();

        prepare_orangepi_uboot_config(
            root.path(),
            &QuickOrangeConfigArgs {
                serial: Some("/dev/ttyUSB1".to_string()),
                baud: Some("115200".to_string()),
                dtb: Some(dtb_path.clone()),
            },
        )
        .unwrap();

        let value: toml::Value = toml::from_str(&fs::read_to_string(tmp_path).unwrap()).unwrap();
        assert_eq!(value["serial"].as_str(), Some("/dev/ttyUSB1"));
        assert_eq!(value["baud_rate"].as_str(), Some("115200"));
        let expected_dtb = dtb_path.display().to_string();
        assert_eq!(value["dtb_file"].as_str(), Some(expected_dtb.as_str()));
    }

    #[test]
    fn prepare_sg2002_uboot_config_updates_existing_tmp_overrides() {
        let root = tempdir().unwrap();
        write_sg2002_uboot_template(root.path());
        let tmp_path = tmp_sg2002_uboot_config_path(root.path());
        fs::create_dir_all(tmp_path.parent().unwrap()).unwrap();
        fs::write(
            &tmp_path,
            "serial = \"/dev/existing\"\nbaud_rate = \"9600\"\n",
        )
        .unwrap();

        prepare_sg2002_uboot_config(
            root.path(),
            &QuickSg2002RunArgs {
                serial: Some("/dev/ttyUSB2".to_string()),
                baud: Some("115200".to_string()),
            },
        )
        .unwrap();

        let value: toml::Value = toml::from_str(&fs::read_to_string(tmp_path).unwrap()).unwrap();
        assert_eq!(value["serial"].as_str(), Some("/dev/ttyUSB2"));
        assert_eq!(value["baud_rate"].as_str(), Some("115200"));
        assert_eq!(value["kernel_load_addr"].as_str(), Some("0x80200000"));
        assert_eq!(value["fit_load_addr"].as_str(), Some("0x82200000"));
        assert_eq!(value["dtb_file"].as_str(), Some("sg2002.dtb"));
    }

    #[test]
    fn refresh_sg2002_config_copies_companion_its() {
        let root = tempdir().unwrap();
        write_sg2002_build_template(root.path());
        write_sg2002_uboot_template(root.path());

        refresh_sg2002_config(root.path()).unwrap();

        assert_eq!(
            fs::read_to_string(tmp_sg2002_build_config_path(root.path()).with_extension("its"))
                .unwrap(),
            "ITS_TEMPLATE"
        );
    }

    #[test]
    fn ensure_sg2002_config_copies_companion_its_for_missing_tmp_build_config() {
        let root = tempdir().unwrap();
        write_sg2002_build_template(root.path());
        write_sg2002_uboot_template(root.path());

        ensure_sg2002_config(root.path()).unwrap();

        assert_eq!(
            fs::read_to_string(tmp_sg2002_build_config_path(root.path()).with_extension("its"))
                .unwrap(),
            "ITS_TEMPLATE"
        );
    }
}
