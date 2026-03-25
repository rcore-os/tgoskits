use std::path::PathBuf;

use clap::{Args as ClapArgs, Subcommand};

use crate::axvisor::context::AxvisorContext;

pub mod config;
pub mod registry;
pub mod storage;

use config::ImageConfig;
use storage::Storage;

#[derive(ClapArgs)]
pub struct Args {
    #[command(flatten)]
    pub overrides: ConfigOverrides,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(ClapArgs, Debug, Clone, Default)]
pub struct ConfigOverrides {
    #[arg(short('S'), long, global = true)]
    pub local_storage: Option<PathBuf>,

    #[arg(short('R'), long, global = true)]
    pub registry: Option<String>,

    #[arg(short('N'), long, global = true)]
    pub no_auto_sync: bool,

    #[arg(long, global = true)]
    pub auto_sync_threshold: Option<u64>,
}

impl ConfigOverrides {
    pub fn apply_on(&self, config: &mut ImageConfig) {
        if let Some(local_storage) = self.local_storage.as_ref() {
            config.local_storage = local_storage.clone();
        }
        if let Some(registry) = self.registry.as_ref() {
            config.registry = registry.clone();
        }
        if self.no_auto_sync {
            config.auto_sync = false;
        }
        if let Some(auto_sync_threshold) = self.auto_sync_threshold {
            config.auto_sync_threshold = auto_sync_threshold;
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// List all available images.
    Ls(ArgsLs),
}

#[derive(ClapArgs)]
pub struct ArgsLs {
    #[arg(short, long)]
    pub verbose: bool,

    pub pattern: Option<String>,
}

pub async fn run(args: Args, ctx: &AxvisorContext) -> anyhow::Result<()> {
    match args.command {
        Command::Ls(ls) => list_images(ctx, &args.overrides, ls).await,
    }
}

async fn list_images(
    ctx: &AxvisorContext,
    overrides: &ConfigOverrides,
    args: ArgsLs,
) -> anyhow::Result<()> {
    let mut config = ImageConfig::read_config(ctx.workspace_root())?;
    overrides.apply_on(&mut config);
    let storage = Storage::new_from_config(&config).await?;
    storage.image_registry.print(args.verbose, args.pattern.as_deref());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_apply_on_config() {
        let mut config = ImageConfig::new_default();
        let overrides = ConfigOverrides {
            local_storage: Some(PathBuf::from("/tmp/custom")),
            registry: Some("https://example.com/registry.toml".to_string()),
            no_auto_sync: true,
            auto_sync_threshold: Some(123),
        };

        overrides.apply_on(&mut config);

        assert_eq!(config.local_storage, PathBuf::from("/tmp/custom"));
        assert_eq!(config.registry, "https://example.com/registry.toml");
        assert!(!config.auto_sync);
        assert_eq!(config.auto_sync_threshold, 123);
    }
}
