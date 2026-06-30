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

#![no_std]

//! This crate provides a minimal VM monitor (VMM) for running guest VMs.
//!
//! This crate contains:
//! - [`AxVM`]: The main structure representing a VM.

extern crate alloc;
#[macro_use]
extern crate log;

mod arch;
pub mod boot;
mod cache;
mod host;
pub mod irq;
pub mod layout;
pub mod lifecycle;
mod manager;
mod percpu;
mod runtime;
mod task;
mod timer;
mod vcpu;
mod vm;

pub mod config;

pub use ax_cpumask::CpuMask;
pub use ax_page_table_entry::MappingFlags;
pub use axdevice_base::{AccessWidth, Port, SysRegAddr};
pub use axvcpu::{AxVCpuExitReason, InterruptTriggerMode, VCpuState};
pub use axvm_types::{GuestPhysAddr, HostPhysAddr, VMId};
pub(crate) use host::{
    paging::HostPagingHandler,
    task::{AxTaskExt, AxTaskRef, TaskInner, WaitQueue, WaitQueueHandle as HostWaitQueueHandle},
};
pub use irq::InterruptFabric;
pub use lifecycle::{StopReason, VmLifecycleError, VmStatus};
pub use manager::{
    AxvmRuntime, current_vcpu_id, current_vm_id, get_vm_by_id, get_vm_list,
    inject_current_vcpu_interrupt, register_vm,
};
#[cfg(target_arch = "loongarch64")]
pub use runtime::loongarch_irq::{
    register_guest_irq_route as register_loongarch_guest_irq_route,
    unregister_guest_irq_routes as unregister_loongarch_guest_irq_routes,
};
pub use task::{AsVCpuTask, VCpuTask};
pub use vm::{AxVCpuRef, AxVM, AxVMRef, FwCfgDeviceConfig, VMMemoryRegion};

/// The architecture-independent per-CPU type.
pub type AxVMPerCpu = axvcpu::AxPerCpu<vcpu::AxVMArchPerCpuImpl>;

/// Check and dispatch pending AxVM timer events on the current CPU.
pub fn check_timer_events() {
    timer::check_events();
}

/// Clean data cache lines covering a host virtual address range.
pub fn clean_dcache_range(addr: ax_memory_addr::VirtAddr, size: usize) {
    cache::clean_dcache_range(addr, size);
}

/// Return the host FDT boot argument physical address.
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub fn host_fdt_bootarg() -> usize {
    host::arceos::host_fdt_bootarg()
}

/// Convert a host physical address into a host virtual address.
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub fn host_phys_to_virt(paddr: ax_memory_addr::PhysAddr) -> ax_memory_addr::VirtAddr {
    host::arceos::phys_to_virt(paddr)
}

/// Shut down ArceOS filesystems so guest passthrough can take ownership.
#[cfg(all(
    any(feature = "fs", feature = "host-fs"),
    any(target_arch = "x86_64", target_arch = "loongarch64")
))]
pub fn shutdown_host_filesystems() -> ax_errno::AxResult {
    host::arceos::shutdown_host_filesystems()
}

/// Register a native host IRQ as the source for one x86 guest IOAPIC GSI.
#[cfg(target_arch = "x86_64")]
pub fn register_x86_ioapic_irq_forwarding_route(guest_gsi: usize, host_irq: irq_framework::IrqId) {
    runtime::register_x86_ioapic_irq_forwarding_route(guest_gsi, host_irq);
}

/// Register a native host IRQ and trigger mode as the source for one x86 guest
/// IOAPIC GSI.
#[cfg(target_arch = "x86_64")]
pub fn register_x86_ioapic_irq_forwarding_route_with_trigger(
    guest_gsi: usize,
    host_irq: irq_framework::IrqId,
    trigger: InterruptTriggerMode,
) {
    runtime::register_x86_ioapic_irq_forwarding_route_with_trigger(guest_gsi, host_irq, trigger);
}

/// Register a callback to activate one x86 guest IOAPIC GSI after the guest has
/// programmed a usable virtual IOAPIC route for it.
#[cfg(target_arch = "x86_64")]
pub fn register_x86_ioapic_irq_forwarding_activator(guest_gsi: usize, activator: fn()) {
    runtime::register_x86_ioapic_irq_forwarding_activator(guest_gsi, activator);
}
