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

use ::ostool::run::qemu::{RunQemuArgs, run_qemu_with_more_default_args};
use anyhow::Result;

use crate::arceos::{
    ArceosConfig, PreparedArtifacts,
    build::{prepare_artifacts, resolve_effective_config},
    context::AxContext,
    ostool as ostool_bridge,
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
        ostool_bridge::build_qemu_default_args(&self.ctx.config, self.ctx.manifest_dir())
    }

    /// Get QEMU binary name
    pub fn qemu_binary(&self) -> String {
        format!("qemu-system-{}", self.ctx.config.arch.to_qemu_arch())
    }

    /// Run QEMU through ostool's cargo_run flow.
    pub async fn run(&self) -> Result<()> {
        let effective_config = resolve_effective_config(
            self.ctx.manifest_dir(),
            self.ctx.app_dir(),
            &self.ctx.config,
        )?;
        let prepared = prepare_artifacts(
            self.ctx.manifest_dir(),
            self.ctx.app_dir(),
            &effective_config,
        )?;
        self.run_prepared(prepared, &effective_config).await
    }

    async fn run_prepared(
        &self,
        prepared: PreparedArtifacts,
        effective_config: &ArceosConfig,
    ) -> Result<()> {
        let mut ctx = prepared.cargo_spec.ctx.into_app_context();
        ctx.config_search_dir = Some(self.ctx.config_search_dir().to_path_buf());
        ostool_bridge::cargo_build_with_artifact_resolution(&mut ctx, &prepared.cargo_spec.cargo)
            .await?;
        run_qemu_with_more_default_args(
            ctx,
            RunQemuArgs {
                qemu_config: self.ctx.qemu_config_path.clone(),
                dtb_dump: false,
                show_output: true,
            },
            ostool_bridge::build_qemu_default_args(effective_config, self.ctx.manifest_dir()),
            effective_config.qemu.success_regex.clone(),
            effective_config.qemu.fail_regex.clone(),
        )
        .await
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

    #[test]
    fn run_qemu_args_forward_explicit_config_path() {
        let config_path = PathBuf::from("/tmp/custom-qemu.toml");
        let args = RunQemuArgs {
            qemu_config: Some(config_path.clone()),
            dtb_dump: false,
            show_output: true,
        };

        assert_eq!(args.qemu_config, Some(config_path));
        assert!(!args.dtb_dump);
        assert!(args.show_output);
    }
}
