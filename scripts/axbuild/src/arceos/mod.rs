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

pub mod build;
pub mod config;
pub mod context;
pub mod features;
pub mod ostool;
pub mod platform;
pub mod qemu;

use std::path::PathBuf;

pub use build::{BuildOutput, Builder, PreparedArtifacts, prepare_artifacts};
pub use config::{
    AVAILABLE_BOARDS, ArceosConfig, ArceosConfigOverride, Arch, BuildMode, LogLevel, NetDev,
    QEMU_CONFIG_FILE_NAME, QemuOptions, apply_defconfig, config_path, load_board_config,
    load_config, parse_qemu_options, resolve_package_app_dir, save_config,
};
pub use context::RunScope;
pub use features::FeatureResolver;
pub use platform::PlatformResolver;
pub use qemu::QemuRunner;

use crate::arceos::context::AxContext;

pub struct AxBuild {
    ctx: AxContext,
}

impl AxBuild {
    pub fn new(ctx: AxContext) -> Self {
        Self { ctx }
    }

    pub fn from_overrides(
        overrides: ArceosConfigOverride,
        package: Option<String>,
        qemu_config_path: Option<PathBuf>,
        run_scope: RunScope,
    ) -> anyhow::Result<Self> {
        let ctx = AxContext::new(overrides, package, qemu_config_path, run_scope)?;
        Ok(Self::new(ctx))
    }

    pub async fn build(self) -> anyhow::Result<BuildOutput> {
        let builder = Builder::new(self.ctx);
        builder.build().await
    }

    pub async fn run_qemu(self) -> anyhow::Result<()> {
        self.run_qemu_internal().await
    }

    pub async fn test(self) -> anyhow::Result<()> {
        self.run_qemu_internal().await
    }

    pub async fn run_qemu_with_config_path(self, qemu_config_path: PathBuf) -> anyhow::Result<()> {
        self.run_qemu_with_config_path_internal(qemu_config_path)
            .await
    }

    pub async fn test_with_config_path(self, qemu_config_path: PathBuf) -> anyhow::Result<()> {
        self.run_qemu_with_config_path_internal(qemu_config_path)
            .await
    }

    async fn run_qemu_internal(self) -> anyhow::Result<()> {
        let qemu_runner = QemuRunner::new(self.ctx);
        qemu_runner.run().await
    }

    async fn run_qemu_with_config_path_internal(
        self,
        qemu_config_path: PathBuf,
    ) -> anyhow::Result<()> {
        let mut ctx = self.ctx;
        ctx.qemu_config_path = Some(qemu_config_path);
        let qemu_runner = QemuRunner::new(ctx);
        qemu_runner.run().await
    }
}
