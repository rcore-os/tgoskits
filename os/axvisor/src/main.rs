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
#[cfg(any(feature = "browser-console", feature = "http-axum"))]
mod http;
mod manager;
#[cfg(feature = "browser-console")]
mod network_console;
#[cfg(feature = "browser-console")]
mod network_status;
#[cfg(feature = "vcpu-perf-load")]
mod perf_load;
mod shell;
#[cfg(feature = "test-virq-delivery")]
mod virq_regression;

/// Axvisor kernel entry point.
///
/// The startup sequence is:
///
/// 1. Configure the sole runtime host-console owner.
/// 2. Print the startup banner through its output worker.
/// 3. Check and enable hardware virtualization on every CPU.
/// 4. Build the default guest VMs.
/// 5. Spawn the management plane first — the configured HTTP and network
///    console services so they are live before any guest boots — then the VM
///    lifecycle waiter and the physical-console shell.
///
fn main() {
    guest_console::configure_host_console()
        .unwrap_or_else(|error| panic!("failed to configure host console: {error:#}"));

    guest_console::submit_host_bytes(banner::STARTUP);

    info!("Starting virtualization...");
    let manager = manager::AxvmManager::new()
        .unwrap_or_else(|error| panic!("failed to initialize AxVM manager: {error:#}"));

    manager.init_default_vms();
    #[cfg(feature = "vcpu-perf-load")]
    let _performance_load = perf_load::start();

    // The browser-console registry snapshots the successfully initialized
    // default VM set exactly once. Initialize it before HTTP so the browser's
    // `/api/consoles` endpoint cannot observe a partially configured layout.
    #[cfg(feature = "browser-console")]
    network_console::start()
        .unwrap_or_else(|error| panic!("failed to initialize browser consoles: {error:#}"));

    // The optional HTTP server accepts connections in a loop and needs its
    // own task so neither the shell nor the VMM blocks it. The console registry
    // is already complete when this task is enqueued, but the server's bind
    // still races guest task scheduling because spawning only enqueues work.
    #[cfg(feature = "browser-console")]
    std::thread::Builder::new()
        .name("axvisor-http".into())
        .spawn(|| {
            if let Err(error) = http::serve() {
                let message = format!(
                    "\r\nAxvisor web console unavailable:\r\n  bind = {}\r\n  error = {error:#}\r\n",
                    http::bind_addr()
                );
                guest_console::submit_host_bytes(message.as_bytes());
            }
        })
        .unwrap_or_else(|error| panic!("failed to start Axvisor HTTP server: {error}"));

    #[cfg(all(feature = "http-axum", not(feature = "browser-console")))]
    std::thread::Builder::new()
        .name("axvisor-http".into())
        .spawn(|| {
            http::serve().unwrap_or_else(|error| panic!("Axvisor HTTP server failed: {error:#}"));
        })
        .unwrap_or_else(|error| panic!("failed to start Axvisor HTTP server: {error}"));

    #[cfg(feature = "browser-console")]
    network_status::start();

    // With `no-auto-start` the default VMs are only created (staying in
    // `Ready`) and the management plane boots them on demand, so nothing is
    // launched or waited on here.
    #[cfg(not(feature = "no-auto-start"))]
    let _ = manager.launch_default_vms();

    #[cfg(feature = "test-virq-delivery")]
    virq_regression::start();

    #[cfg(not(feature = "no-auto-start"))]
    std::thread::Builder::new()
        .name("axvisor-vm-wait".into())
        .spawn(manager::AxvmManager::wait_for_default_vms)
        .unwrap_or_else(|error| panic!("failed to start VM completion waiter: {error}"));

    #[cfg(not(feature = "no-auto-start"))]
    info!("[OK] Default guest initialized");

    info!("shell task on CPU{}", axvm::host::cpu::current_id());

    shell::console_init();
}
