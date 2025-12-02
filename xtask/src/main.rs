#![cfg_attr(not(any(windows, unix)), no_main)]
#![cfg_attr(not(any(windows, unix)), no_std)]
#![cfg(any(windows, unix))]

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use std::fs;
use std::path::{Path, PathBuf};

mod cargo;
mod clippy;
mod ctx;
mod devspace;
mod image;
mod menuconfig;
mod tbuld;
mod vmconfig;

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "ArceOS build configuration management tool")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set default build configuration from board configs
    Defconfig {
        /// Board configuration name (e.g., qemu-aarch64, orangepi-5-plus, phytiumpi)
        board_name: String,
    },
    /// Build the ArceOS project with current configuration
    Build,
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
    Image(image::ImageArgs),
    /// Manage local devspace dependencies
    Devspace(DevspaceArgs),
}

#[derive(Parser)]
struct QemuArgs {
    /// Path to custom build configuration file (TOML format)
    #[arg(long)]
    build_config: Option<PathBuf>,
    
    /// Path to custom QEMU configuration file
    #[arg(long)]
    qemu_config: Option<PathBuf>,
    
    /// Comma-separated list of VM configuration files
    #[arg(long)]
    vmconfigs: Vec<String>,
}

#[derive(Parser)]
struct ClippyArgs {
    /// Only check specific packages (comma separated)
    #[arg(long)]
    packages: Option<String>,
    
    /// Only check specific targets (comma separated)
    #[arg(long)]
    targets: Option<String>,
    
    /// Continue on error instead of exiting immediately
    #[arg(long)]
    continue_on_error: bool,
    
    /// Dry run - show what would be checked without running clippy
    #[arg(long)]
    dry_run: bool,
    
    /// Automatically fix clippy warnings where possible
    #[arg(long)]
    fix: bool,
    
    /// Allow fixing when the working directory is dirty (has uncommitted changes)
    #[arg(long)]
    allow_dirty: bool,
}

#[derive(Parser)]
struct UbootArgs {
    /// Path to custom build configuration file (TOML format)
    #[arg(long)]
    build_config: Option<PathBuf>,
    
    /// Path to custom U-Boot configuration file
    #[arg(long)]
    uboot_config: Option<PathBuf>,
    
    /// Comma-separated list of VM configuration files
    #[arg(long)]
    vmconfigs: Vec<String>,
}

#[derive(Args)]
struct DevspaceArgs {
    #[command(subcommand)]
    action: DevspaceCommand,
}

#[derive(Subcommand)]
enum DevspaceCommand {
    /// Start the development workspace
    Start,
    
    /// Stop the development workspace
    Stop,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut ctx = ctx::Context::new();

    match cli.command {
        Commands::Defconfig { board_name } => {
            defconfig_command(&board_name)?;
        }
        Commands::Build => {
            println!("Building the project...");
            ctx.run_build().await?;
            println!("Build completed successfully.");
        }
        Commands::Clippy(args) => {
            clippy::run_clippy(args)?;
        }
        Commands::Qemu(args) => {
            ctx.vmconfigs = args.vmconfigs;
            ctx.build_config_path = args.build_config;
            ctx.run_qemu(args.qemu_config).await?;
        }
        Commands::Uboot(args) => {
            ctx.vmconfigs = args.vmconfigs;
            ctx.build_config_path = args.build_config;
            ctx.run_uboot(args.uboot_config).await?;
        }
        Commands::Vmconfig => {
            ctx.run_vmconfig().await?;
        }
        Commands::Menuconfig => {
            ctx.run_menuconfig().await?;
        }
        Commands::Image(args) => {
            image::run_image(args).await?;
        }
        Commands::Devspace(args) => match args.action {
            DevspaceCommand::Start => devspace::start()?,
            DevspaceCommand::Stop => devspace::stop()?,
        },
    }

    Ok(())
}

fn defconfig_command(board_name: &str) -> Result<()> {
    println!("Setting default configuration for board: {board_name}");

    // Validate board configuration exists
    let board_config_path = format!("configs/board/{board_name}.toml");
    if !Path::new(&board_config_path).exists() {
        return Err(anyhow!(
            "Board configuration '{board_name}' not found. Available boards: qemu-aarch64, orangepi-5-plus"
        ));
    }

    // Backup existing .build.toml if it exists
    backup_existing_config()?;

    // Copy board configuration to .build.toml
    copy_board_config(board_name)?;

    println!("Successfully set default configuration to: {board_name}");
    Ok(())
}

fn backup_existing_config() -> Result<()> {
    let build_config_path = ".build.toml";

    if Path::new(build_config_path).exists() {
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_path = format!("{build_config_path}.backup_{timestamp}");

        fs::copy(build_config_path, &backup_path)
            .with_context(|| format!("Failed to backup {build_config_path} to {backup_path}"))?;

        println!("Backed up existing configuration to: {backup_path}");
    }

    Ok(())
}

fn copy_board_config(board_name: &str) -> Result<()> {
    let source_path = format!("configs/board/{board_name}.toml");
    let target_path = ".build.toml";

    fs::copy(&source_path, target_path)
        .with_context(|| format!("Failed to copy {source_path} to {target_path}"))?;

    println!("Copied board configuration from: {source_path}");
    Ok(())
}
