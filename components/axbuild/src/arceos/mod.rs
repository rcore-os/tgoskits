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

use std::path::{Path, PathBuf};

pub use build::{
    BuildOutput, Builder, PreparedArtifacts, prepare_artifacts,
    prepare_artifacts_with_qemu_config_path,
};
pub use config::{
    AVAILABLE_BOARDS, AXCONFIG_FILE_NAME, ArceosConfig, ArceosConfigOverride, Arch, BuildMode,
    CONFIG_FILE_NAME, LogLevel, NetDev, OSTOOL_EXTRA_CONFIG_FILE_NAME, QEMU_CONFIG_FILE_NAME,
    QemuOptions, apply_defconfig, axconfig_path, config_path, load_board_config, load_config,
    ostool_extra_config_path, parse_qemu_options, qemu_config_path, resolve_package_app_dir,
    save_config,
};
pub use features::FeatureResolver;
pub use platform::{CpuInfo, PlatformInfo, PlatformResolver};
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
    ) -> anyhow::Result<Self> {
        let ctx = AxContext::new(
            overrides,
            package,
            qemu_config_path,
        )?;
        Ok(Self::new(ctx))
    }

    pub async fn build(self) -> anyhow::Result<BuildOutput> {
        let builder = Builder::new(self.ctx);
        builder.build().await
    }

    pub async fn run_qemu(self) -> anyhow::Result<()> {
        let qemu_runner = QemuRunner::new(self.ctx);
        qemu_runner.run().await
    }

    pub async fn run_qemu_with_config_path(self, qemu_config_path: PathBuf) -> anyhow::Result<()> {
        let mut ctx = self.ctx;
        ctx.qemu_config_path = Some(qemu_config_path);
        let qemu_runner = QemuRunner::new(ctx);
        qemu_runner.run().await
    }
}
