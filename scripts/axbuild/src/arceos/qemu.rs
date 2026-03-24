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

use anyhow::Result;

use crate::arceos::{
    PreparedArtifacts,
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
        self.run_prepared(prepared).await
    }

    async fn run_prepared(&self, prepared: PreparedArtifacts) -> Result<()> {
        let mut tool = prepared.cargo_spec.ctx.into_tool()?;
        let qemu_config_path = ostool_bridge::resolve_external_qemu_config_path(
            self.ctx.manifest_dir(),
            self.ctx.config_search_dir(),
            &self.ctx.config,
            self.ctx.qemu_config_path.as_deref(),
        )?;
        ostool_bridge::cargo_run_qemu(&mut tool, &prepared.cargo_spec.cargo, qemu_config_path).await
    }
}
