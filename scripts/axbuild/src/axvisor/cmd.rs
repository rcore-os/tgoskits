use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "ArceOS build configuration management tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Set default build configuration from board configs
    Defconfig {
        /// Board configuration name (e.g., qemu-aarch64, orangepi-5-plus, phytiumpi)
        board_name: String,
    },
    /// Build the ArceOS project with current configuration
    Build(BuildArgs),
    /// Run clippy checks across all targets and feature combinations
    Clippy(ClippyArgs),
    /// Run ArceOS in QEMU emulation environment
    Qemu(QemuArgs),
    /// Run ArceOS with U-Boot bootloader
    Uboot(UbootArgs),
    /// Generate VM configuration schema
    Vmconfig,
    /// Interactive menu-based configuration editor
    Menuconfig,
    /// Guest Image management
    Image(super::image::ImageArgs),
    /// Manage local devspace dependencies
    Devspace(DevspaceArgs),
}

#[derive(Parser)]
pub struct QemuArgs {
    /// Path to custom build configuration file (TOML format)
    #[arg(long)]
    pub build_config: Option<PathBuf>,

    /// Path to custom QEMU configuration file
    #[arg(long)]
    pub qemu_config: Option<PathBuf>,

    /// Comma-separated list of VM configuration files
    #[arg(long)]
    pub vmconfigs: Vec<String>,

    #[command(flatten)]
    pub build: BuildArgs,
}

#[derive(Parser)]
pub struct ClippyArgs {
    /// Only check specific packages (comma separated)
    #[arg(long)]
    pub packages: Option<String>,

    /// Only check specific targets (comma separated)
    #[arg(long)]
    pub targets: Option<String>,

    /// Continue on error instead of exiting immediately
    #[arg(long)]
    pub continue_on_error: bool,

    /// Dry run - show what would be checked without running clippy
    #[arg(long)]
    pub dry_run: bool,

    /// Automatically fix clippy warnings where possible
    #[arg(long)]
    pub fix: bool,

    /// Allow fixing when the working directory is dirty (has uncommitted changes)
    #[arg(long)]
    pub allow_dirty: bool,
}

#[derive(Parser)]
pub struct UbootArgs {
    /// Path to custom build configuration file (TOML format)
    #[arg(long)]
    pub build_config: Option<PathBuf>,

    /// Path to custom U-Boot configuration file
    #[arg(long)]
    pub uboot_config: Option<PathBuf>,

    /// Comma-separated list of VM configuration files
    #[arg(long)]
    pub vmconfigs: Vec<String>,

    #[command(flatten)]
    pub build: BuildArgs,
}

#[derive(Args)]
pub struct BuildArgs {
    #[arg(long)]
    pub build_dir: Option<PathBuf>,
    #[arg(long)]
    pub bin_dir: Option<PathBuf>,
}

#[derive(Args)]
pub struct DevspaceArgs {
    #[command(subcommand)]
    pub action: DevspaceCommand,
}

#[derive(Subcommand)]
pub enum DevspaceCommand {
    /// Start the development workspace
    Start,
    /// Stop the development workspace
    Stop,
}
