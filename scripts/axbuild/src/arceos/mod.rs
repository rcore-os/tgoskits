use std::{
    ops::{Deref, DerefMut},
    path::PathBuf,
};

use clap::{Args, Subcommand};

use crate::context::{AppContext, QemuConfig};

pub mod build;

/// ArceOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build ArceOS application
    Build(ArgsBuild),
    /// Build and run ArceOS application
    Qemu(ArgsQemu),
}

#[derive(Args)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(short, long)]
    pub package: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,
    #[arg(long)]
    pub plat_dyn: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,
}

pub struct ArceOS {
    app: AppContext,
}

impl ArceOS {
    pub fn new() -> Self {
        let app = AppContext::new();
        Self { app }
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => {
                self.build(args).await?;
            }
            Command::Qemu(args) => {
                self.qemu(args).await?;
            }
        }
        Ok(())
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        self.app.build().await?;
        Ok(())
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        self.app.qemu::<build::BuildConfig>(args.into()).await?;
        Ok(())
    }
}

impl Default for ArceOS {
    fn default() -> Self {
        Self::new()
    }
}

impl From<ArgsQemu> for QemuConfig {
    fn from(args: ArgsQemu) -> Self {
        Self {
            build_config: args.build.config,
            qemu_config: args.qemu_config,
        }
    }
}
