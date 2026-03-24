use std::{fmt::Debug, path::PathBuf};

use ostool::{
    Tool, ToolConfig,
    build::{CargoRunnerKind, config::Cargo},
};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};

#[derive(Debug, Clone)]
pub struct QemuConfig {
    pub build_config: Option<PathBuf>,
    pub qemu_config: Option<PathBuf>,
}

pub trait IBuildConfig:
    Clone + Debug + Default + JsonSchema + DeserializeOwned + Serialize
{
    fn to_cargo_config(self) -> Cargo;
}

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    qemu_config_path: Option<PathBuf>,
    root: PathBuf,
}

impl AppContext {
    pub fn new() -> Self {
        let workspace_root = find_workspace_root();
        println!("Workspace root: {}", workspace_root.display());
        let tool = Tool::new(ToolConfig::default()).unwrap();
        Self {
            tool,
            build_config_path: None,
            qemu_config_path: None,
            root: workspace_root,
        }
    }

    pub async fn build(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn qemu<T: IBuildConfig>(&mut self, qemu_config: QemuConfig) -> anyhow::Result<()> {
        let config = self.perper_config::<T>(qemu_config).await?;

        let kind = CargoRunnerKind::Qemu {
            qemu_config: self.qemu_config_path.clone(),
            debug: false,
            dtb_dump: false,
        };

        self.tool.cargo_run(&config, &kind).await?;

        Ok(())
    }

    async fn perper_build_config<T: IBuildConfig>(
        &mut self,
        build_config: Option<PathBuf>,
    ) -> anyhow::Result<Cargo> {
        let build_config_path = build_config.unwrap_or_else(|| self.root.join(".build.toml"));

        println!("Using build config: {}", build_config_path.display());

        if build_config_path.exists() {
            println!("Found build config at {}", build_config_path.display());
        } else {
            println!(
                "Build config not found at {}, using default config",
                build_config_path.display()
            );
            // Write default config to the path
            let default_build = T::default();
            let toml_str = toml::to_string_pretty(&default_build)?;
            std::fs::write(&build_config_path, toml_str)?;
            println!(
                "Default build config written to {}",
                build_config_path.display()
            );
        }

        let config = toml::from_str::<T>(&std::fs::read_to_string(&build_config_path)?)?;
        let cargo = config.to_cargo_config();

        self.build_config_path = Some(build_config_path);

        Ok(cargo)
    }

    async fn perper_config<T: IBuildConfig>(
        &mut self,
        config: QemuConfig,
    ) -> anyhow::Result<Cargo> {
        let cargo = self.perper_build_config::<T>(config.build_config).await?;

        Ok(cargo)
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new()
    }
}

fn find_workspace_root() -> PathBuf {
    let cargo = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .expect("Failed to get cargo metadata");

    cargo.workspace_root.canonicalize().unwrap()
}
