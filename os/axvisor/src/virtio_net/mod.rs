//! AxVisor OS glue for the virtio-net MMIO device.
//!
//! This module owns everything that is specific to running the device under
//! AxVisor: the device adapter that plugs into the AxVM MMIO router, the factory
//! that builds it from a config row, the deterministic UDP-echo virtual peer and
//! the RX worker lifecycle, and the [`PrepareProfile`] that rebuilds the factory
//! registry + interrupt fabric on every prepare generation.
//!
//! The generic capabilities (VM-backed guest memory accessor, VM-local queued
//! IRQ sink, worker task facade, repeatable prepare profile) live in `axvm`;
//! `axvirtio-net` stays OS-agnostic. Only this module depends on both (plan 2.3).

pub mod adapter;
pub mod backend;
pub mod config;
pub mod factory;
pub mod raw_uplink;
pub mod worker;

// The pure layer-2 switch core (typed port ids, MAC table, forwarding decision)
// lives in the `axvirtio-switch` workspace crate so its deterministic forwarding
// tests run on the host instead of being gated behind the AxVisor QEMU run.
// Modules here import it directly as `axvirtio_switch::...`.

use alloc::sync::{Arc, Weak};

use axdevice::{DeviceFactoryRegistry, register_builtin_factories};
use axvm::{AxVM, AxVmResult, InterruptFabric, PrepareProfile, VmQueuedIrqSink};
use axvm_types::VMInterruptMode;

pub use worker::{start_for_vm as start_workers_for_vm, stop_for_vm as stop_workers_for_vm};

/// Per-VM prepare profile that rebuilds the virtio-net factory registry and the
/// VM-local interrupt fabric for each generation.
///
/// Stored on the VM via [`axvm::AxVM::install_prepare_profile`] before the first
/// prepare, so reset and stopped-start re-prepare the virtio-net glue at a new
/// generation instead of dropping it to the empty default registry.
pub struct VirtioNetPrepareProfile {
    vm: Weak<AxVM>,
}

impl VirtioNetPrepareProfile {
    /// Creates a profile reaching `vm` for guest memory and IRQ routing.
    ///
    /// Callers pass a [`Weak`] (not a strong reference) so the profile cannot
    /// keep the VM alive.
    pub fn new(vm: Weak<AxVM>) -> Self {
        Self { vm }
    }
}

impl PrepareProfile for VirtioNetPrepareProfile {
    fn interrupt_mode(&self) -> VMInterruptMode {
        VMInterruptMode::Emulated
    }

    fn build(&self, generation: usize) -> AxVmResult<(DeviceFactoryRegistry, InterruptFabric)> {
        let mut factories = DeviceFactoryRegistry::new();
        register_builtin_factories(&mut factories)?;
        factories.register(Arc::new(factory::VirtioNetDeviceFactory::new(
            self.vm.clone(),
            generation,
        )))?;

        // Single-vCPU smoke target: route every device IRQ to vCPU 0. Multi-vCPU
        // support requires an affinity/routing policy and is intentionally out
        // of scope for the first version (plan section 2 / stage 2).
        let sink = Arc::new(VmQueuedIrqSink::new(self.vm.clone(), generation, 0));
        let fabric = InterruptFabric::with_sink(VMInterruptMode::Emulated, sink)?;

        Ok((factories, fabric))
    }

    fn on_stopped(&self) {
        if let Some(vm) = self.vm.upgrade() {
            stop_workers_for_vm(vm.id());
        }
    }
}
