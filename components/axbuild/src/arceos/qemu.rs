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

use std::path::{Path, PathBuf};

use ::ostool::build::CargoRunnerKind;
use anyhow::Result;

use crate::arceos::{
    build::prepare_artifacts,
    config::{ArceosConfig, qemu_config_path_for_config},
    ostool as ostool_bridge,
};

/// QEMU runner
pub struct QemuRunner {
    config: ArceosConfig,
    manifest_dir: PathBuf,
}

impl QemuRunner {
    pub fn new(config: ArceosConfig, manifest_dir: PathBuf) -> Self {
        Self {
            config,
            manifest_dir,
        }
    }

    pub fn manifest_dir(&self) -> &Path {
        &self.manifest_dir
    }

    /// Build QEMU command arguments
    pub fn build_args(&self) -> Vec<String> {
        ostool_bridge::build_qemu_config(&self.config, &self.manifest_dir).args
    }

    /// Get QEMU binary name
    pub fn qemu_binary(&self) -> String {
        format!("qemu-system-{}", self.config.arch.to_qemu_arch())
    }

    pub fn qemu_config_path(&self) -> PathBuf {
        qemu_config_path_for_config(&self.manifest_dir, &self.config)
    }

    /// Run QEMU through ostool's cargo_run flow.
    pub async fn run(&self) -> Result<()> {
        let prepared = prepare_artifacts(&self.manifest_dir, &self.config)?;
        let mut ctx = prepared.cargo_spec.ctx.into_app_context();
        ctx.cargo_run(
            &prepared.cargo_spec.cargo,
            &CargoRunnerKind::Qemu {
                qemu_config: Some(prepared.qemu_config_path),
                debug: false,
                dtb_dump: false,
            },
        )
        .await?;
        Ok(())
    }

    /// Get QEMU command as a string (for debugging)
    pub fn command_string(&self) -> String {
        let qemu = self.qemu_binary();
        let args = self.build_args();
        format!("{} {}", qemu, args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::arceos::config::{Arch, NetDev, QemuOptions};

    #[test]
    fn test_qemu_binary() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("/tmp/project"));
        assert_eq!(runner.qemu_binary(), "qemu-system-aarch64");
    }

    #[test]
    fn test_cpu_type() {
        let config = ArceosConfig {
            arch: Arch::X86_64,
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("/tmp/project"));

        let args = runner.build_args();
        assert!(args.windows(2).any(|w| w[0] == "-cpu" && w[1] == "max"));
        assert!(!args.iter().any(|arg| arg == "-kernel"));
    }

    #[test]
    fn test_memory_custom() {
        let config = ArceosConfig {
            arch: Arch::AArch64,
            mem: Some("256M".to_string()),
            ..Default::default()
        };
        let runner = QemuRunner::new(config, PathBuf::from("/tmp/project"));

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
        let runner = QemuRunner::new(config, PathBuf::from("/tmp/project"));

        let args = runner.build_args();
        assert!(args.iter().any(|a| a.contains("user")));
    }

    #[test]
    fn test_qemu_config_path() {
        let config = ArceosConfig::default();
        let runner = QemuRunner::new(config, PathBuf::from("/workspace/os/arceos"));

        assert_eq!(
            runner.qemu_config_path(),
            PathBuf::from("/workspace/os/arceos/examples/helloworld/.qemu.toml")
        );
    }
}
