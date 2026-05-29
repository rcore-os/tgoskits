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

//! # Axvisor Kernel
//!
//! Kernel entry point for the Axvisor hypervisor.
//!
//! This module wires together early boot presentation, hardware virtualization
//! enablement, VM initialization/startup, and the interactive management shell.
//! The implementation is intentionally small so that the boot order is visible
//! from a single file.

#![no_std]
#![no_main]
#![cfg(target_os = "none")]

#[macro_use]
extern crate log;

#[macro_use]
extern crate alloc;

extern crate ax_std as std;

mod config;
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
mod fdt;
mod images;
mod manager;
mod shell;

use std::println;

/// Startup banners printed before the hypervisor begins initialization.
const LOGO: [&str; 2] = [
    r#"
       d8888            888     888  d8b
      d88888            888     888  Y8P
     d88P888            888     888
    d88P 888  888  888  Y88b   d88P  888  .d8888b    .d88b.   888d888
   d88P  888  `Y8bd8P'   Y88b d88P   888  88K       d88""88b  888P"
  d88P   888    X88K      Y88o88P    888  "Y8888b.  888  888  888
 d8888888888  .d8""8b.     Y888P     888       X88  Y88..88P  888
d88P     888  888  888      Y8P      888   88888P'   "Y88P"   888
"#,
    r#"
    _         __     ___
   / \   __  _\ \   / (_)___  ___  _ __
  / _ \  \ \/ /\ \ / /| / __|/ _ \| '__|
 / ___ \  >  <  \ V / | \__ \ (_) | |
/_/   \_\/_/\_\  \_/  |_|___/\___/|_|
"#,
];

/// Prints the startup banner to the console.
fn print_logo() {
    println!();
    println!("{}", LOGO[0]);
    println!();
    println!("by AxVisor Team");
    println!();
}

/// Axvisor kernel entry point.
///
/// The startup sequence is:
///
/// 1. Print the startup banner.
/// 2. Check and enable hardware virtualization on every CPU.
/// 3. Build and start configured guest VMs.
/// 4. Enter the management shell after the default guests have exited.
#[unsafe(no_mangle)]
fn main() {
    print_logo();

    info!("Starting virtualization...");
    let manager = manager::AxvmManager::new().expect("failed to initialize AxVM manager");

    manager.init_default_vms();
    manager.start_default_vms();

    info!("[OK] Default guest initialized");

    shell::console_init();
}
