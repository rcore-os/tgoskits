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

pub trait IBuildConfig: Clone + Debug + JsonSchema + DeserializeOwned + Serialize {
    fn to_cargo_config(self) -> anyhow::Result<Cargo>;
}

pub struct AppContext {
    tool: Tool,
    build_config_path: Option<PathBuf>,
    qemu_config_path: Option<PathBuf>,
    root: PathBuf,
}

impl AppContext {
    pub fn new() -> anyhow::Result<Self> {
        let workspace_root = find_workspace_root();
        crate::logging::init_logging(&workspace_root)?;

        info!("Workspace root: {}", workspace_root.display());

        let tool = Tool::new(ToolConfig::default()).unwrap();
        Ok(Self {
            tool,
            build_config_path: None,
            qemu_config_path: None,
            root: workspace_root,
        })
    }

    pub async fn build(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn qemu<T: IBuildConfig>(
        &mut self,
        qemu_config: QemuConfig,
        def_config: T,
        config_ext: &str,
    ) -> anyhow::Result<()> {
        let config = self
            .perper_qemu_config::<T>(qemu_config, def_config, config_ext)
            .await?;

        let kind = CargoRunnerKind::Qemu {
            qemu_config: self.qemu_config_path.clone(),
            debug: false,
            dtb_dump: false,
        };

        self.tool.cargo_run(&config, &kind).await?;

        Ok(())
    }

    pub async fn uboot<T: IBuildConfig>(
        &mut self,
        build_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        def_config: T,
        config_ext: &str,
    ) -> anyhow::Result<()> {
        let cargo = self
            .perper_build_config(build_config, def_config, config_ext)
            .await?;

        let kind = CargoRunnerKind::Uboot { uboot_config };

        self.tool.cargo_run(&cargo, &kind).await?;

        Ok(())
    }

    async fn perper_build_config<T: IBuildConfig>(
        &mut self,
        build_config: Option<PathBuf>,
        def_config: T,
        ext: &str,
    ) -> anyhow::Result<Cargo> {
        let ext = if ext.is_empty() {
            String::new()
        } else {
            format!("-{}", ext)
        };

        let build_name = format!(".build{ext}.toml");

        let build_config_path = build_config.unwrap_or_else(|| self.root.join(&build_name));

        println!("Using build config: {}", build_config_path.display());

        if build_config_path.exists() {
            info!("Found build config at {}", build_config_path.display());
        } else {
            info!(
                "Build config not found at {}, using default config",
                build_config_path.display()
            );
            // Write default config to the path
            let default_build = def_config;
            let toml_str = toml::to_string_pretty(&default_build)?;
            std::fs::write(&build_config_path, toml_str)?;
            info!(
                "Default build config written to {}",
                build_config_path.display()
            );
        }

        let config = toml::from_str::<T>(&std::fs::read_to_string(&build_config_path)?)?;
        let cargo = config.to_cargo_config()?;

        self.build_config_path = Some(build_config_path);

        Ok(cargo)
    }

    async fn perper_qemu_config<T: IBuildConfig>(
        &mut self,
        config: QemuConfig,
        def_config: T,
        config_ext: &str,
    ) -> anyhow::Result<Cargo> {
        self.qemu_config_path = config.qemu_config;
        let cargo = self
            .perper_build_config::<T>(config.build_config, def_config, config_ext)
            .await?;

        Ok(cargo)
    }
}

impl Default for AppContext {
    fn default() -> Self {
        Self::new().expect("failed to initialize AppContext")
    }
}

fn find_workspace_root() -> PathBuf {
    let cargo = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .expect("Failed to get cargo metadata");

    cargo.workspace_root.canonicalize().unwrap()
}
