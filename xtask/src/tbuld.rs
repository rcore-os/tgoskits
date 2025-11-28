use std::path::PathBuf;

use anyhow::Context as _;
use ostool::build::config::{Cargo, LogLevel};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

use crate::ctx::Context;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Config {
    /// target triple
    pub target: String,
    /// features to enable
    pub features: Vec<String>,
    /// log level feature
    pub log: Option<LogLevel>,
    /// other cargo args
    pub cargo_args: Vec<String>,
    /// whether to output as binary
    pub to_bin: bool,
    pub smp: Option<usize>,
    pub vm_configs: Vec<String>,
}

impl Context {
    pub fn load_config(&mut self) -> anyhow::Result<Cargo> {
        let json = schema_for!(Config);

        let mut config_path = self.ctx.workspace_folder.join(".build.toml");
        if let Some(c) = &self.build_config_path {
            config_path = c.clone();
        }

        std::fs::write(
            config_path.parent().unwrap().join(".build-schema.json"),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .with_context(|| "Failed to write schema file .build-schema.json")?;

        let config_str = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;
        let config: Config = toml::from_str(&config_str)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;

        self.ctx.build_config_path = Some(config_path);

        let mut vm_configs = config.vm_configs.to_vec();
        vm_configs.extend(self.vmconfigs.iter().cloned());

        let mut vm_config_paths = vec![];
        for vm_config in &vm_configs {
            let mut vm_config = PathBuf::from(vm_config);
            if !vm_config.is_absolute() {
                vm_config = self.ctx.workspace_folder.join(vm_config);
            }
            if !vm_config.exists() {
                return Err(anyhow::anyhow!(
                    "VM config file '{}' does not exist.",
                    vm_config.display()
                ));
            }
            vm_config_paths.push(vm_config);
        }

        let mut cargo = Cargo {
            target: config.target,
            package: "axvisor".to_string(),
            features: config.features,
            log: config.log,
            args: config.cargo_args,
            to_bin: config.to_bin,
            ..Default::default()
        };

        if let Some(smp) = config.smp {
            cargo.env.insert("AXVISOR_SMP".to_string(), smp.to_string());
        }

        if !vm_config_paths.is_empty() {
            let value = std::env::join_paths(&vm_config_paths)
                .map_err(|e| anyhow::anyhow!("Failed to join VM config paths: {e}"))?
                .to_string_lossy()
                .into_owned();
            cargo.env.insert("AXVISOR_VM_CONFIGS".to_string(), value);
        }

        Ok(cargo)
    }

    pub async fn run_build(&mut self) -> anyhow::Result<()> {
        let config = self.load_config()?;
        self.ctx.build_cargo(&config).await?;

        Ok(())
    }
}
