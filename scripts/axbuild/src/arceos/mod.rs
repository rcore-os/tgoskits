use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::context::{AppContext, BuildConfigLookupKey, QemuConfig};

pub mod build;

/// ArceOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build ArceOS application
    Build(ArgsBuild),
    /// Build and run ArceOS application
    Qemu(ArgsQemu),

    /// Build and run ArceOS application with U-Boot
    Uboot(ArgsUboot),
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
    pub no_dyn: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

pub struct ArceOS {
    app: AppContext,
}

fn build_lookup_key(args: &ArgsBuild) -> BuildConfigLookupKey {
    BuildConfigLookupKey::new("arceos", args.package.clone(), args.target.clone())
}

impl ArceOS {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => {
                self.build(args).await?;
            }
            Command::Qemu(args) => {
                self.qemu(args).await?;
            }
            Command::Uboot(args) => {
                self.uboot(args).await?;
            }
        }
        Ok(())
    }

    async fn build(&mut self, _args: ArgsBuild) -> anyhow::Result<()> {
        self.app.build().await?;
        Ok(())
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let lookup_key = build_lookup_key(&args.build);
        let def_config = build::BuildConfig::new(
            args.build.target.clone(),
            args.build.package.clone(),
            args.build.no_dyn,
        );

        self.app.qemu(args.into(), def_config, lookup_key).await?;
        Ok(())
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let lookup_key = build_lookup_key(&args.build);
        let def_config = build::BuildConfig::new(
            args.build.target.clone(),
            args.build.package.clone(),
            args.build.no_dyn,
        );
        self.app
            .uboot(args.build.config, args.uboot_config, def_config, lookup_key)
            .await?;
        Ok(())
    }
}

impl Default for ArceOS {
    fn default() -> Self {
        Self::new().expect("failed to initialize ArceOS")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_map_to_lookup_key() {
        let args = ArgsBuild {
            config: None,
            package: Some("arceos-helloworld".to_string()),
            target: Some("aarch64-unknown-none-softfloat".to_string()),
            no_dyn: false,
        };

        let key = build_lookup_key(&args);

        assert_eq!(
            key,
            BuildConfigLookupKey::new(
                "arceos",
                Some("arceos-helloworld".to_string()),
                Some("aarch64-unknown-none-softfloat".to_string())
            )
        );
    }
}
