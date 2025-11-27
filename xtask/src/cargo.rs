use std::{fs, path::PathBuf};

use ostool::run::cargo::CargoRunnerKind;

use crate::ctx::Context;

impl Context {
    pub async fn run_qemu(&mut self, config_path: Option<PathBuf>) -> anyhow::Result<()> {
        let build_config = self.load_config()?;
        
        let arch = if build_config.target.contains("aarch64") {
            Arch::Aarch64
        } else if build_config.target.contains("x86_64") {
            Arch::X86_64
        } else {
            return Err(anyhow::anyhow!(
                "Unsupported target architecture: {}",
                build_config.target
            ));
        };
        
        let config_path = if let Some(path) = config_path {
            path
        } else {
            PathBuf::from(format!(".qemu-{arch:?}.toml").to_lowercase())
        };

        // 如果配置文件不存在，从默认位置复制
        if !config_path.exists() {
            fs::copy(
                PathBuf::from("scripts")
                    .join("ostool")
                    .join(format!("qemu-{arch:?}.toml").to_lowercase()),
                &config_path,
            )?;
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

        let config_path = config_path.unwrap_or_else(|| PathBuf::from(".uboot.toml"));

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
}
