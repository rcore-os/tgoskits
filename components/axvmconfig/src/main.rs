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

//! axvmconfig - ArceOS-Hypervisor VM Configuration Tool.
//!
//! This is the main entry point for the axvmconfig command-line tool.
//! The tool provides functionality to validate and generate VM configuration
//! files for the ArceOS hypervisor system.
#![cfg_attr(not(all(feature = "std", any(windows, unix))), no_main)]
#![cfg_attr(not(all(feature = "std", any(windows, unix))), no_std)]

#[cfg(all(feature = "std", any(windows, unix)))]
use axvmconfig::*;

// CLI tool module - only available with std feature.
#[cfg(all(feature = "std", any(windows, unix)))]
mod tool;

// Template generation module - only available with std feature.
#[cfg(all(feature = "std", any(windows, unix)))]
mod templates;

/// Main entry point for the axvmconfig CLI tool.
///
/// Sets up logging and delegates to the tool module for command processing.
/// The tool supports two main operations:
/// - Validating existing TOML configuration files
/// - Generating new configuration templates from command-line parameters
#[cfg(all(feature = "std", any(windows, unix)))]
fn main() {
    // Configure logger with debug level for development
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .init();

    // Run the CLI tool
    tool::run();
}

#[cfg(not(all(feature = "std", any(windows, unix))))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(not(all(feature = "std", any(windows, unix))))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
