// Copyright 2025 The tgoskits Team
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

use ::ostool::{
    ctx::{AppContext, OutputArtifacts, PathConfig},
    run::qemu::{RunQemuArgs, run_qemu},
};
use anyhow::{Context, Result};
use object::Architecture as ObjectArchitecture;

use crate::arceos::{
    config::{ArceosConfig, Arch},
    ostool as ostool_bridge,
};

/// QEMU runner
pub struct QemuRunner {
    config: ArceosConfig,
    image_path: PathBuf,
    arceos_dir: PathBuf,
}

impl QemuRunner {
    pub fn new(config: ArceosConfig, image_path: PathBuf, arceos_dir: PathBuf) -> Self {
        Self {
            config,
            image_path,
            arceos_dir,
        }
    }

    /// Build QEMU command arguments
    pub fn build_args(&self) -> Vec<String> {
        ostool_bridge::build_qemu_config(&self.config, &self.arceos_dir).args
    }

    /// Get QEMU binary name
    pub fn qemu_binary(&self) -> String {
        format!("qemu-system-{}", self.config.arch.to_qemu_arch())
    }

    pub fn qemu_config_path(&self) -> PathBuf {
        self.arceos_dir.join(".qemu.toml")
    }

    /// Run QEMU
    pub async fn run(&self) -> Result<()> {
        let qemu = self.qemu_binary();
        let qemu_config = ostool_bridge::build_qemu_config(&self.config, &self.arceos_dir);
        let qemu_config_path = self.qemu_config_path();

        std::fs::write(&qemu_config_path, toml::to_string_pretty(&qemu_config)?)
            .with_context(|| format!("Failed to write {}", qemu_config_path.display()))?;

        tracing::info!("Running QEMU: {} {}", qemu, qemu_config.args.join(" "));

        run_qemu(
            self.app_context(),
            RunQemuArgs {
                qemu_config: Some(qemu_config_path),
                dtb_dump: false,
                show_output: true,
            },
        )
        .await
        .with_context(|| format!("Failed to run {}", qemu))?;

        Ok(())
    }

    /// Get QEMU command as a string (for debugging)
    pub fn command_string(&self) -> String {
        let qemu = self.qemu_binary();
        let args = self.build_args();
        format!("{} {}", qemu, args.join(" "))
    }

    fn app_context(&self) -> AppContext {
        let workspace_root = self.workspace_root();
        AppContext {
            paths: PathConfig {
                workspace: workspace_root.clone(),
                manifest: workspace_root,
                artifacts: OutputArtifacts {
                    elf: None,
                    bin: Some(self.image_path.clone()),
                },
                ..Default::default()
            },
            arch: Some(object_arch(self.config.arch)),
            ..Default::default()
        }
    }

    fn workspace_root(&self) -> PathBuf {
        self.arceos_dir
            .parent()
            .and_then(|path| path.parent())
            .map_or_else(|| self.arceos_dir.clone(), |path| path.to_path_buf())
    }
}

fn object_arch(arch: Arch) -> ObjectArchitecture {
    match arch {
        Arch::X86_64 => ObjectArchitecture::X86_64,
        Arch::AArch64 => ObjectArchitecture::Aarch64,
        Arch::RiscV64 => ObjectArchitecture::Riscv64,
        Arch::LoongArch64 => ObjectArchitecture::LoongArch64,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::arceos::config::{ArceosConfig, NetDev, QemuOptions};

    #[test]
    fn test_qemu_binary() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));
        assert_eq!(runner.qemu_binary(), "qemu-system-aarch64");
    }

    #[test]
    fn test_cpu_type() {
        let config = ArceosConfig {
            arch: Arch::X86_64,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.windows(2).any(|w| w[0] == "-cpu" && w[1] == "max"));
        assert!(!args.iter().any(|arg| arg == "-kernel"));
    }

    #[test]
    fn test_memory_default() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            mem: None,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "128M"));
    }

    #[test]
    fn test_memory_custom() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            mem: Some("256M".to_string()),
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.windows(2).any(|w| w[0] == "-m" && w[1] == "256M"));
    }

    #[test]
    fn test_network_user() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            qemu: QemuOptions {
                net: true,
                net_dev: NetDev::User,
                ..Default::default()
            },
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("test.elf"), PathBuf::from("/tmp"));

        let args = runner.build_args();
        assert!(args.iter().any(|a| a.contains("user")));
    }

    #[test]
    fn test_qemu_config_path() {
        let config = ArceosConfig::default();
        let runner = QemuRunner::new(
            config,
            PathBuf::from("test.bin"),
            PathBuf::from("/workspace/os/arceos"),
        );

        assert_eq!(
            runner.qemu_config_path(),
            PathBuf::from("/workspace/os/arceos/.qemu.toml")
        );
    }
}
