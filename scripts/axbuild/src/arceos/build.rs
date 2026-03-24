use std::collections::HashMap;

use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::context::IBuildConfig;

#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize)]
pub struct BuildConfig {
    /// Environment variables to set during the build.
    pub env: HashMap<String, String>,
    /// Target triple (e.g., "aarch64-unknown-none-softfloat", "riscv64gc-unknown-none-elf").
    pub target: String,
    /// Package name to build.
    pub package: String,
    /// Cargo features to enable.
    pub features: Vec<String>,
    /// Log level feature to automatically enable.
    pub log: LogLevel,
    /// Whether to use dynamic platform.
    pub plat_dyn: bool,
}

impl BuildConfig {
    fn resolve_features(&mut self) {
        // Platform-related features
        if self.plat_dyn {
            self.features.push("axstd/plat-dyn".to_string());
        } else {
            if !self.features.contains(&"myplat".to_string()) {
                self.features.push("defplat".to_string());
            }
        }

        self.features.sort();
        self.features.dedup();
    }

    fn perper_env(&mut self) {
        self.env
            .insert("AX_LOG".into(), format!("{:?}", self.log).to_lowercase());
    }

    fn build_cargo_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        args.push("--config".to_string());
        args.push(if self.plat_dyn {
            format!(
                "target.{}.rustflags=[\"-Clink-arg=-Taxplat.x\"]",
                self.target
            )
        } else {
            format!(
                "target.{}.rustflags=[\"-Clink-arg=-Tlinker.x\",\"-Clink-arg=-no-pie\",\"\
                 -Clink-arg=-znostart-stop-gc\"]",
                self.target
            )
        });
        args
    }
}

impl Default for BuildConfig {
    fn default() -> Self {
        let mut env = HashMap::new();
        env.insert("AX_IP".to_string(), "10.0.2.15".to_string());
        env.insert("AX_GW".to_string(), "10.0.2.2".to_string());

        Self {
            env,
            target: "aarch64-unknown-none-softfloat".to_string(),
            package: "arceos-helloworld".to_string(),
            plat_dyn: true,
            log: LogLevel::Info,
            features: vec!["axstd".to_string()],
        }
    }
}

impl IBuildConfig for BuildConfig {
    fn to_cargo_config(mut self) -> Cargo {
        let args = self.build_cargo_args();
        self.resolve_features();

        Cargo {
            env: self.env,
            target: self.target,
            package: self.package,
            features: self.features,
            log: Some(self.log),
            extra_config: None,
            args,
            pre_build_cmds: vec![],
            post_build_cmds: vec![],
            to_bin: true,
        }
    }
}
