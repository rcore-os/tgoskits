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

use ::ostool::build::CargoRunnerKind;
use anyhow::Result;

use crate::arceos::{
    PreparedArtifacts, build::prepare_artifacts_with_qemu_config_path,
    config::QEMU_CONFIG_FILE_NAME, context::AxContext, ostool as ostool_bridge,
};

/// QEMU runner
pub struct QemuRunner {
    ctx: AxContext,
}

impl QemuRunner {
    pub fn new(ctx: AxContext) -> Self {
        Self { ctx }
    }

    /// Build QEMU command arguments
    pub fn build_args(&self) -> Vec<String> {
        ostool_bridge::build_qemu_config(&self.ctx.config, self.ctx.manifest_dir()).args
    }

    /// Get QEMU binary name
    pub fn qemu_binary(&self) -> String {
        format!("qemu-system-{}", self.ctx.config.arch.to_qemu_arch())
    }

    pub fn qemu_config_path(&self) -> PathBuf {
        self.ctx
            .qemu_config_path
            .clone()
            .unwrap_or_else(|| self.ctx.app_dir().join(QEMU_CONFIG_FILE_NAME))
    }

    /// Run QEMU through ostool's cargo_run flow.
    pub async fn run(&self) -> Result<()> {
        let qemu_config_path = self.qemu_config_path();
        let prepared = prepare_artifacts_with_qemu_config_path(
            self.ctx.manifest_dir(),
            self.ctx.app_dir(),
            &self.ctx.config,
            qemu_config_path,
        )?;
        self.run_prepared(prepared).await
    }

    async fn run_prepared(&self, prepared: PreparedArtifacts) -> Result<()> {
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
