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
#[cfg(feature = "http-axum")]
mod http;
mod manager;
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
/// 5. Spawn the management plane first — the HTTP server so the API is live
///    before any guest boots — then the VM lifecycle waiter and the shell.
///
/// The vCPU tasks are pinned to the secondary CPUs via `phys_cpu_ids` in the
/// VM configs, while the management console stays on the primary CPU.
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

    // The management HTTP server accepts connections in a loop and needs its
    // own task so neither the shell nor the VMM blocks it. It is spawned first
    // so the management API is ready as early as possible. `ax_std::thread::spawn`
    // only enqueues the task — the main task keeps running until it yields or
    // blocks — so the server's bind does not necessarily happen before
    // `launch_default_vms` queues the vCPU tasks; the ordering is best-effort.
    #[cfg(feature = "http-axum")]
    std::thread::Builder::new()
        .name("axvisor-http".into())
        .spawn(http::serve)
        .unwrap_or_else(|error| panic!("failed to start management HTTP server: {error}"));

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

    // The management console runs on the primary CPU (Core 0) while the vCPU
    // tasks are pinned to Core 1 via `phys_cpu_ids`, so it stays responsive
    // regardless of guest behavior.
    info!("shell task on CPU{}", axvm::host::cpu::current_id());

    shell::console_init();
}
