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
mod guest_console;
mod guest_restart;
mod host_noise;
mod manager;
mod shell;

#[cfg(feature = "rk3588-npu-handoff")]
const NPU_HANDOFF_MARKER_COPIES: usize = 5;
#[cfg(feature = "rk3588-npu-handoff")]
const NPU_HANDOFF_MARKER_INTERVAL_MS: u64 = 100;

#[cfg(any(feature = "backtrace", feature = "test-panic-no-backtrace"))]
fn init_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        eprintln!("{info}");
        // When the `backtrace` feature is NOT enabled, axbacktrace is compiled
        // without `alloc` → Inner::Disabled → BT_ERROR requires_alloc.
        // When the `backtrace` feature IS enabled, axbacktrace captures real
        // frames (alloc=true, frames enumerated).
        eprintln!("{}", axbacktrace::Backtrace::capture().kind("panic"));
    }));
}

/// Axvisor kernel entry point.
///
/// The startup sequence is:
///
/// 1. Print the startup banner.
/// 2. Check and enable hardware virtualization on every CPU.
/// 3. Build and start configured guest VMs.
/// 4. Run the VM completion waiter and management console concurrently.
fn main() {
    #[cfg(any(feature = "backtrace", feature = "test-panic-no-backtrace"))]
    init_panic_hook();

    // Test-only panic paths — gated behind dedicated features so they never
    // activate in normal builds.  These are consumed by test-suit cases that
    // verify the backtrace markers (or their absence) via QEMU regex matching.
    #[cfg(feature = "test-backtrace-panic")]
    panic!("axvisor backtrace smoke test: deliberate panic to verify backtrace output");
    #[cfg(feature = "test-panic-no-backtrace")]
    panic!("axvisor no-backtrace smoke test: panic without backtrace");

    banner::print_logo();

    info!("Starting virtualization...");
    #[cfg(feature = "rk3588-npu-handoff")]
    ax_driver::soc::require_rk3588_npu_handoff();
    #[cfg(feature = "rk3588-npu-handoff")]
    write_rk3588_npu_handoff_markers();
    let manager = manager::AxvmManager::new()
        .unwrap_or_else(|error| panic!("failed to initialize AxVM manager: {error:#}"));

    manager.init_default_vms();
    let guest_restart = guest_restart::GuestRestartTask::start_configured()
        .unwrap_or_else(|error| panic!("failed to start guest restart: {error:#}"));
    let host_noise = host_noise::HostNoiseTask::start_configured()
        .unwrap_or_else(|error| panic!("failed to start host interference: {error:#}"));
    let default_vms = manager::AxvmManager::vm_list();
    guest_console::configure_host_console_reader(&default_vms)
        .unwrap_or_else(|error| panic!("failed to configure host console input: {error:#}"));
    let started_vms = manager.launch_default_vms();
    guest_console::attach_default(started_vms);

    std::thread::Builder::new()
        .name("axvisor-vm-wait".into())
        .spawn(move || {
            manager::AxvmManager::wait_for_default_vms();
            if let Some(guest_restart) = guest_restart {
                guest_restart
                    .join_and_publish()
                    .unwrap_or_else(|error| panic!("guest restart validation failed: {error:#}"));
            }
            if let Some(host_noise) = host_noise {
                host_noise.stop_and_publish().unwrap_or_else(|error| {
                    panic!("host interference validation failed: {error:#}")
                });
            }
        })
        .unwrap_or_else(|error| panic!("failed to start VM completion waiter: {error}"));

    info!("[OK] Default guest initialized");

    shell::console_init();
}

#[cfg(feature = "rk3588-npu-handoff")]
fn write_rk3588_npu_handoff_markers() {
    for copy_index in 0..NPU_HANDOFF_MARKER_COPIES {
        ax_driver::soc::report_rk3588_npu_handoff();
        if copy_index + 1 < NPU_HANDOFF_MARKER_COPIES {
            // Early boot logs from multiple CPUs share this UART. Spacing the
            // copies keeps a complete contract observable across a lossy burst.
            std::thread::sleep(core::time::Duration::from_millis(
                NPU_HANDOFF_MARKER_INTERVAL_MS,
            ));
        }
    }
}
