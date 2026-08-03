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

#[macro_use]
extern crate log;

#[macro_use]
extern crate alloc;

use ax_std as _;

mod banner;
mod config;
mod guest_restart;
mod host_noise;
mod manager;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
mod platform_irq;
mod shell;

/// Axvisor kernel entry point.
///
/// The startup sequence is:
///
/// 1. Print the startup banner.
/// 2. Check and enable hardware virtualization on every CPU.
/// 3. Build and start configured guest VMs.
/// 4. Enter the management shell after the default guests have exited.
fn main() {
    banner::print_logo();

    info!("Starting virtualization...");
    let manager = manager::AxvmManager::new()
        .unwrap_or_else(|error| panic!("failed to initialize AxVM manager: {error:#}"));

    manager.init_default_vms();
    let guest_restart = guest_restart::GuestRestartTask::start_configured()
        .unwrap_or_else(|error| panic!("failed to start guest restart: {error:#}"));
    let host_noise = host_noise::HostNoiseTask::start_configured()
        .unwrap_or_else(|error| panic!("failed to start host interference: {error:#}"));
    manager.start_default_vms();
    if let Some(guest_restart) = guest_restart {
        guest_restart
            .join_and_publish()
            .unwrap_or_else(|error| panic!("guest restart validation failed: {error:#}"));
    }
    if let Some(host_noise) = host_noise {
        host_noise
            .stop_and_publish()
            .unwrap_or_else(|error| panic!("host interference validation failed: {error:#}"));
    }

    info!("[OK] Default guest initialized");

    shell::console_init();
}
