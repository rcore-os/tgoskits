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

use std::{fs, path::PathBuf};

use ostool::build::CargoRunnerKind;

use super::{ctx::Context, default_qemu_config_path, resolve_repo_path};

impl Context {
    pub async fn run_qemu(&mut self, config_path: Option<PathBuf>) -> anyhow::Result<()> {
        let build_config = self.load_config()?;

        let arch = if build_config.target.contains("aarch64") {
            Arch::Aarch64
        } else if build_config.target.contains("x86_64") {
            Arch::X86_64
        } else if build_config.target.contains("riscv64") {
            Arch::Riscv64
        } else {
            return Err(anyhow::anyhow!(
                "Unsupported target architecture: {}",
                build_config.target
            ));
        };

        let config_path = if let Some(path) = config_path {
            resolve_repo_path(self.repo_root(), path)
        } else {
            self.repo_root()
                .join(format!(".qemu-{}.toml", arch.as_str()))
        };
        let default_config_path = default_qemu_config_path(self.repo_root(), arch.as_str());

        // If the configuration file does not exist, copy from the default location
        if !config_path.exists() {
            fs::copy(&default_config_path, &config_path)?;
        }

        let kind = CargoRunnerKind::Qemu {
            qemu_config: Some(config_path),
            debug: false,
            dtb_dump: false,
        };

        self.ctx.cargo_run(&build_config, &kind).await?;

        Ok(())
    }

    pub async fn run_uboot(&mut self, config_path: Option<PathBuf>) -> anyhow::Result<()> {
        let build_config = self.load_config()?;

        let config_path = config_path
            .map(|path| resolve_repo_path(self.repo_root(), path))
            .unwrap_or_else(|| self.repo_root().join(".uboot.toml"));

        let kind = CargoRunnerKind::Uboot {
            uboot_config: Some(config_path),
        };

        self.ctx.cargo_run(&build_config, &kind).await?;

        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum Arch {
    Aarch64,
    X86_64,
    Riscv64,
}

impl Arch {
    fn as_str(self) -> &'static str {
        match self {
            Arch::Aarch64 => "aarch64",
            Arch::X86_64 => "x86_64",
            Arch::Riscv64 => "riscv64",
        }
    }
}
