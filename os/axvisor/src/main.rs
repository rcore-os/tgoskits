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
#[cfg(feature = "web")]
mod control;
mod guest_console;
mod guest_images;
mod manager;
mod net_uplink;
#[cfg(feature = "vcpu-perf-load")]
mod perf_load;
mod shell;
mod sync;
#[cfg(feature = "test-virq-delivery")]
mod virq_regression;

/// Axvisor kernel entry point.
///
/// The startup sequence is:
///
/// 1. Configure the sole runtime host-console owner.
/// 2. Print the startup banner through its output worker.
/// 3. Check and enable hardware virtualization on every CPU.
/// 4. Build the default guest VMs, then report the pool of configs the
///    management plane may start on demand.
/// 5. Spawn the management plane first — the configured HTTP service and the
///    registry watcher that feeds the browser UI — so they are live before any
///    guest boots, then the VM lifecycle waiter and the physical-console shell.
///
fn main() {
    // The boot instant is recorded before anything else so the control plane's
    // uptime counts from the kernel entry point rather than from the first
    // request that happens to ask for it.
    #[cfg(feature = "web")]
    control::domain::host::mark_boot();

    guest_console::configure_host_console()
        .unwrap_or_else(|error| panic!("failed to configure host console: {error:#}"));

    guest_console::submit_host_bytes(banner::STARTUP);

    axvisor::builtin::prepare_root()
        .unwrap_or_else(|error| panic!("failed to prepare Axvisor boot resources: {error:#}"));

    info!("Starting virtualization...");
    let manager = manager::AxvmManager::new()
        .unwrap_or_else(|error| panic!("failed to initialize AxVM manager: {error:#}"));

    // Bridge guest virtio-net ports onto the selected host interface before any
    // guest device is created, so the host DHCP/console stack keeps owning the
    // wire and guest MACs are validated against the reserved host MACs.
    net_uplink::start();
    manager
        .init_default_vms()
        .unwrap_or_else(|error| panic!("failed to initialize default VMs: {error:#}"));
    #[cfg(feature = "vcpu-perf-load")]
    let _performance_load = perf_load::start();

    // The pool reports what it found under the guest tree: a config there
    // becomes a VM only when the shell or the control plane asks for it.
    #[cfg(feature = "web")]
    control::domain::pool::log_startup_state();

    // The registry watcher behind `/ws/events` is started before HTTP so the
    // first subscriber cannot miss a change.
    #[cfg(feature = "web")]
    control::domain::events::start();

    // The optional HTTP server accepts connections in a loop and needs its
    // own task so neither the shell nor the VMM blocks it. The server's bind
    // still races guest task scheduling because spawning only enqueues work.
    #[cfg(feature = "web")]
    std::thread::Builder::new()
        .name("axvisor-http".into())
        .spawn(|| {
            if let Err(error) = control::serve() {
                let message = format!(
                    "\r\nAxvisor web console unavailable:\r\n  bind = {}\r\n  error = {error:#}\r\n",
                    control::bind_addr()
                );
                guest_console::submit_host_bytes(message.as_bytes());
            }
        })
        .unwrap_or_else(|error| panic!("failed to start Axvisor HTTP server: {error}"));

    #[cfg(feature = "web")]
    control::network_status::start();

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
        .spawn(move || manager.wait_for_default_vms())
        .unwrap_or_else(|error| panic!("failed to start VM completion waiter: {error}"));

    #[cfg(not(feature = "no-auto-start"))]
    info!("[OK] Default guest initialized");

    info!("shell task on CPU{}", axvm::host::cpu::current_id());

    shell::console_init();
}
