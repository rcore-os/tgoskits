//! TUI-based menu configuration system.
//!
//! This module provides an interactive terminal user interface for configuring
//! build options, similar to Linux kernel's menuconfig. It supports editing
//! configuration for:
//!
//! - Build settings (`.build.toml`)
//! - QEMU settings (`.qemu.toml`)
//! - U-Boot settings (`.uboot.toml`)

use anyhow::Context;
use anyhow::Result;
use clap::ValueEnum;
use log::info;
use tokio::fs;

use crate::Tool;
use crate::build::config::BuildConfig;
use crate::run::qemu::QemuConfig;
use crate::run::uboot::UbootConfig;
use crate::utils::PathResultExt;

/// Menu configuration mode selector.
#[derive(ValueEnum, Clone, Debug)]
pub enum MenuConfigMode {
    /// Configure QEMU runner settings.
    Qemu,
    /// Configure U-Boot runner settings.
    Uboot,
}

/// Handler for menu configuration operations.
pub struct MenuConfigHandler;

impl MenuConfigHandler {
    /// Handles the menu configuration command.
    ///
    /// # Arguments
    ///
    /// * `tool` - The tool instance.
    /// * `mode` - Optional mode specifying which configuration to edit.
    ///   If `None`, shows the default build configuration menu.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be loaded or saved.
    pub async fn handle_menuconfig(tool: &mut Tool, mode: Option<MenuConfigMode>) -> Result<()> {
        match mode {
            Some(MenuConfigMode::Qemu) => {
                Self::handle_qemu_config(tool).await?;
            }
            Some(MenuConfigMode::Uboot) => {
                Self::handle_uboot_config(tool).await?;
            }
            None => {
                Self::handle_default_config(tool).await?;
            }
        }
        Ok(())
    }

    async fn handle_default_config(tool: &mut Tool) -> Result<()> {
        let config_path = tool.resolve_build_config_path(None);
        tool.ctx_mut().build_config_path = Some(config_path.clone());

        let config = jkconfig::run::<BuildConfig>(config_path.clone(), true, &tool.ui_hooks())
            .await
            .with_context(|| format!("failed to load build config: {}", config_path.display()))?;

        if let Some(config) = config {
            tool.ctx_mut().build_config = Some(config);
        } else {
            println!("\n未更改构建配置");
        }

        Ok(())
    }

    async fn handle_qemu_config(tool: &mut Tool) -> Result<()> {
        info!("配置 QEMU 运行参数");

        let config_path = crate::run::qemu::resolve_qemu_config_path(tool, None)?;

        if config_path.exists() {
            println!("\n当前 QEMU 配置文件: {}", config_path.display());
        } else {
            println!("\n未找到 QEMU 配置文件，将使用默认配置");
        }

        let config = jkconfig::run::<QemuConfig>(config_path.clone(), true, &[])
            .await
            .with_context(|| format!("failed to load QEMU config: {}", config_path.display()))?;

        if let Some(c) = config {
            fs::write(&config_path, toml::to_string_pretty(&c)?)
                .await
                .with_path("failed to write file", &config_path)?;
            println!("\nQEMU 配置已保存到 {}", config_path.display());
        } else {
            println!("\n未更改 QEMU 配置");
        }

        Ok(())
    }

    async fn handle_uboot_config(tool: &mut Tool) -> Result<()> {
        info!("配置 U-Boot 运行参数");

        println!("=== U-Boot 配置模式 ===");

        // 检查是否存在 U-Boot 配置文件
        let uboot_config_path = tool.workspace_dir().join(".uboot.toml");
        if uboot_config_path.exists() {
            println!("\n当前 U-Boot 配置文件: {}", uboot_config_path.display());
            // 这里可以读取并显示当前的 U-Boot 配置
        } else {
            println!("\n未找到 U-Boot 配置文件，将使用默认配置");
        }
        let config = jkconfig::run::<UbootConfig>(uboot_config_path.clone(), true, &[])
            .await
            .with_context(|| {
                format!(
                    "failed to load U-Boot config: {}",
                    uboot_config_path.display()
                )
            })?;
        if let Some(c) = config {
            fs::write(&uboot_config_path, toml::to_string_pretty(&c)?)
                .await
                .with_path("failed to write file", &uboot_config_path)?;
            println!("\nU-Boot 配置已保存到 .uboot.toml");
        } else {
            println!("\n未更改 U-Boot 配置");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::build::config::BuildConfig;
    use jkconfig::data::menu::MenuRoot;
    use jkconfig::data::types::ElementType;
    use schemars::schema_for;

    #[test]
    fn test_log_field_is_enum_not_oneof() {
        let schema = schema_for!(BuildConfig);
        let menu = MenuRoot::try_from(schema.as_value()).expect("schema parse ok");

        fn find_log(elem: &ElementType) -> Option<&ElementType> {
            match elem {
                ElementType::Menu(m) => m.children.iter().find_map(|c| find_log(c)),
                ElementType::OneOf(o) => o.variants.iter().find_map(|v| find_log(v)),
                ElementType::Item(item) if item.base.key().ends_with("log") => Some(elem),
                _ => None,
            }
        }

        let log_elem = find_log(&menu.menu).expect("log field should exist");
        match log_elem {
            ElementType::Item(item) => {
                assert!(
                    matches!(&item.item_type, jkconfig::data::item::ItemType::Enum(e) if e.variants.len() == 5),
                    "log should be Enum with 5 variants, got: {:?}",
                    item.item_type
                );
            }
            other => panic!("log should be Item(Enum), got: {:?}", other),
        }
    }
}
