//! axvmconfig - ArceOS-Hypervisor VM Configuration Tool.
//!
//! This is the main entry point for the axvmconfig command-line tool.
//! The tool provides functionality to validate and generate VM configuration
//! files for the ArceOS hypervisor system.
#![cfg_attr(not(feature = "std"), no_std)]

use axvmconfig::*;

// CLI tool module - only available with std feature.
#[cfg(feature = "std")]
mod tool;

// Template generation module - only available with std feature.
#[cfg(feature = "std")]
mod templates;

/// Main entry point for the axvmconfig CLI tool.
///
/// Sets up logging and delegates to the tool module for command processing.
/// The tool supports two main operations:
/// - Validating existing TOML configuration files
/// - Generating new configuration templates from command-line parameters
fn main() {
    // Configure logger with debug level for development
    #[cfg(feature = "std")]
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .init();

    // Run the CLI tool
    #[cfg(feature = "std")]
    tool::run();
}
