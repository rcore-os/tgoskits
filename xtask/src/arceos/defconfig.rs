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

use std::{fs, path::Path};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;

/// Available board configurations
pub const AVAILABLE_BOARDS: &[&str] = &["qemu-x86_64", "qemu-aarch64", "qemu-riscv64"];

/// Set default build configuration from board configs
pub fn run_defconfig(board_name: &str) -> Result<()> {
    println!("Setting default configuration for board: {}", board_name);

    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("failed to locate workspace root")?;

    // Validate board configuration exists
    let source = if board_name.ends_with(".toml") {
        format!("os/arceos/configs/board/{}", board_name)
    } else {
        format!("os/arceos/configs/board/{}.toml", board_name)
    };

    let source_path = workspace_root.join(&source);
    if !source_path.exists() {
        return Err(anyhow!(
            "Board configuration '{}' not found at {}\nAvailable boards: {}",
            board_name,
            source_path.display(),
            AVAILABLE_BOARDS.join(", ")
        ));
    }

    // Backup existing .build.toml if it exists
    let build_config_path = workspace_root.join("os/arceos/.build.toml");
    backup_existing_config(&build_config_path)?;

    // Copy board configuration to .build.toml
    let target_path = workspace_root.join("os/arceos/.build.toml");
    fs::copy(&source_path, &target_path).with_context(|| {
        format!(
            "Failed to copy {} to {}",
            source_path.display(),
            target_path.display()
        )
    })?;

    println!("Successfully set default configuration to: {}", board_name);
    println!("Config file: {}", target_path.display());

    Ok(())
}

/// Backup existing configuration file
fn backup_existing_config(build_config_path: &Path) -> Result<()> {
    if build_config_path.exists() {
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let backup_path = build_config_path.with_extension(format!("toml.backup_{}", timestamp));

        fs::copy(build_config_path, &backup_path).with_context(|| {
            format!(
                "Failed to backup {} to {}",
                build_config_path.display(),
                backup_path.display()
            )
        })?;

        println!(
            "Backed up existing configuration to: {}",
            backup_path.display()
        );
    }

    Ok(())
}
