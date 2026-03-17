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

use anyhow::{Result, anyhow};
use axbuild::arceos::{AVAILABLE_BOARDS, apply_defconfig, config_path};

/// Set default build configuration from board configs
pub fn run_defconfig(board_name: &str) -> Result<()> {
    println!("Setting default configuration for board: {}", board_name);

    let manifest_dir = super::config::arceos_manifest_dir()?;
    if !AVAILABLE_BOARDS.contains(&board_name) {
        return Err(anyhow!(
            "Board configuration '{}' not found\nAvailable boards: {}",
            board_name,
            AVAILABLE_BOARDS.join(", ")
        ));
    }

    let _config = apply_defconfig(&manifest_dir, board_name)?;

    println!("Successfully set default configuration to: {}", board_name);
    println!("Config file: {}", config_path(&manifest_dir).display());

    Ok(())
}
