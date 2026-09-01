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
#[cfg(any(
    feature = "test-console-atomic-output",
    feature = "test-console-interleave"
))]
mod console_regression;
mod guest_console;
#[cfg(any(feature = "browser-console", feature = "http-axum"))]
mod http;
mod manager;
#[cfg(feature = "browser-console")]
mod network_console;
#[cfg(feature = "browser-console")]
mod network_status;
mod shell;

#[cfg(any(feature = "backtrace", feature = "test-panic-no-backtrace"))]
fn init_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // When the `backtrace` feature is NOT enabled, axbacktrace is compiled
        // without `alloc` → Inner::Disabled → BT_ERROR requires_alloc.
        // When the `backtrace` feature IS enabled, axbacktrace captures real
        // frames (alloc=true, frames enumerated).
        let backtrace = axbacktrace::Backtrace::capture().kind("panic");
        let _ = ax_std::os::arceos::modules::ax_runtime::emergency_console::write_fmt(
            format_args!("{info}\n{backtrace}\n"),
        );
    }));
}

#[cfg(feature = "test-console-atomic-output")]
fn init_atomic_output_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let _ = ax_std::os::arceos::modules::ax_runtime::emergency_console::write_fmt(
            format_args!("{info}\n"),
        );
    }));
}

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
    #[cfg(feature = "test-console-atomic-output")]
    init_atomic_output_panic_hook();
    #[cfg(any(feature = "backtrace", feature = "test-panic-no-backtrace"))]
    init_panic_hook();

    // Test-only panic paths — gated behind dedicated features so they never
    // activate in normal builds.  These are consumed by test-suit cases that
    // verify the backtrace markers (or their absence) via QEMU regex matching.
    #[cfg(feature = "test-backtrace-panic")]
    panic!("axvisor backtrace smoke test: deliberate panic to verify backtrace output");
    #[cfg(feature = "test-panic-no-backtrace")]
    panic!("axvisor no-backtrace smoke test: panic without backtrace");

    guest_console::configure_host_console()
        .unwrap_or_else(|error| panic!("failed to configure host console: {error:#}"));

    guest_console::submit_host_bytes(banner::STARTUP);

    info!("Starting virtualization...");
    let manager = manager::AxvmManager::new()
        .unwrap_or_else(|error| panic!("failed to initialize AxVM manager: {error:#}"));

    manager.init_default_vms();

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

    #[cfg(feature = "test-console-atomic-output")]
    console_regression::emit_atomic_output();

    #[cfg(feature = "test-console-interleave")]
    console_regression::emit_interleave();

    // With `no-auto-start` the default VMs are only created (staying in
    // `Ready`) and the management plane boots them on demand, so nothing is
    // launched or waited on here.
    #[cfg(not(feature = "no-auto-start"))]
    let started_vms = manager.launch_default_vms();
    #[cfg(not(feature = "no-auto-start"))]
    guest_console::attach_default(started_vms);

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
