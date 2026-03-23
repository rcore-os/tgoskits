// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::path::PathBuf;

use anyhow::Context as _;
use ostool::build::config::{Cargo, LogLevel};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

use super::{build_config_path, build_schema_path, ctx::Context, resolve_repo_path};

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

        let mut config_path = build_config_path(self.repo_root());
        if let Some(c) = &self.build_config_path {
            config_path = resolve_repo_path(self.repo_root(), c);
        }

        let path = build_schema_path(&config_path);

        std::fs::write(&path, serde_json::to_string_pretty(&json).unwrap())
            .with_context(|| format!("Failed to write schema file: {}", path.display()))?;

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
                vm_config = self.repo_root().join(vm_config);
            }
            if !vm_config.exists() {
                return Err(anyhow::anyhow!(
                    "VM config file '{}' does not exist.",
                    vm_config.display()
                ));
            }
            vm_config_paths.push(vm_config);
        }

        let log_level = config
            .log
            .as_ref()
            .map(|l| format!("{:?}", l).to_lowercase());

        let mut cargo = Cargo {
            target: config.target,
            package: "axvisor".to_string(),
            features: config.features,
            log: config.log,
            args: vec!["--bin".to_string(), "axvisor".to_string()],
            to_bin: config.to_bin,
            ..Default::default()
        };
        if cargo.features.iter().any(|feature| feature == "dyn-plat") {
            // Dynamic-platform AArch64 builds link as PIE, so core/alloc must be
            // rebuilt with the same PIC settings instead of using prebuilt std.
            ensure_cargo_arg_pair(&mut cargo.args, "-Z", "build-std=core,alloc");
            ensure_cargo_arg_pair(
                &mut cargo.args,
                "-Z",
                "build-std-features=compiler-builtins-mem",
            );
            ensure_cargo_arg_pair(
                &mut cargo.args,
                "--config",
                &format!(
                    "target.{}.rustflags=[\"-Clink-arg=-Taxplat.x\"]",
                    cargo.target
                ),
            );
        }
        if !cargo.features.iter().any(|feature| feature == "dyn-plat") {
            ensure_cargo_arg_pair(
                &mut cargo.args,
                "--config",
                &format!(
                    "target.{}.rustflags=[\"-Clink-arg=-Tlinker.x\",\"-Clink-arg=-no-pie\",\"\
                     -Clink-arg=-znostart-stop-gc\"]",
                    cargo.target
                ),
            );
        }
        cargo.args.extend(config.cargo_args);

        if let Some(smp) = config.smp {
            cargo.env.insert("AXVISOR_SMP".to_string(), smp.to_string());
        }

        if let Some(log_level) = log_level {
            cargo.env.insert("AX_LOG".to_string(), log_level);
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
        self.ctx.cargo_build(&config).await?;

        Ok(())
    }
}

fn ensure_cargo_arg_pair(args: &mut Vec<String>, flag: &str, value: &str) {
    if args
        .windows(2)
        .any(|window| window[0] == flag && window[1] == value)
    {
        return;
    }
    args.push(flag.to_string());
    args.push(value.to_string());
}
