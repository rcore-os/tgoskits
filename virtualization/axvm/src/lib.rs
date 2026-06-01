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
mod cache;
mod host;
mod manager;
mod percpu;
mod runtime;
mod task;
mod timer;
mod vcpu;
mod vm;

pub mod config;

pub use ax_cpumask::CpuMask;
pub use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags, device::AccessWidth};
pub use axhvc::{HyperCallCode, HyperCallResult};
pub use axvcpu::{AxVCpuExitReason, InterruptTriggerMode, VCpuState};
/// Paging handler backed by AxVM's private host adapter.
pub use host::paging::HostPagingHandler;
pub(crate) use host::task::{
    AxTaskExt, AxTaskRef, CurrentTask, TaskInner, WaitQueue, WaitQueueHandle as HostWaitQueueHandle,
};
pub use manager::{
    AxvmRuntime, current_vcpu_id, current_vm_id, get_vm_by_id, get_vm_list,
    inject_current_vcpu_interrupt, register_vm, setup_primary_vcpu,
};
#[cfg(target_arch = "riscv64")]
pub use riscv_vcpu::GprIndex as RiscvGprIndex;
pub use task::{AsVCpuTask, VCpuTask};
pub use vm::{AxVCpuRef, AxVM, AxVMRef, VMMemoryRegion, VMStatus};

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

/// Dispatch a host IRQ vector through the ArceOS IRQ handler.
#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn dispatch_host_irq(vector: usize) {
    host::arceos::dispatch_host_irq(vector);
}

/// Build an ArceOS host CPU mask from raw bits.
pub(crate) fn host_cpu_mask_from_raw_bits(bits: usize) -> host::task::CpuMask {
    host::task::cpu_mask_from_raw_bits(bits)
}

/// Return the current host task.
pub(crate) fn current_host_task() -> CurrentTask {
    host::task::current_task()
}

/// Spawn a prepared host task.
pub(crate) fn spawn_host_task(task: TaskInner) -> AxTaskRef {
    host::task::spawn_task(task)
}

/// Wait on a host wait queue until `condition` becomes true.
pub(crate) fn host_wait_queue_wait_until(
    queue: &HostWaitQueueHandle,
    condition: impl Fn() -> bool,
) {
    host::task::wait_queue_wait_until(queue, condition);
}

/// Wake tasks waiting on a host wait queue.
pub(crate) fn host_wait_queue_wake(queue: &HostWaitQueueHandle, count: u32) {
    host::task::wait_queue_wake(queue, count);
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
#[cfg(all(any(feature = "fs", feature = "host-fs"), target_arch = "x86_64"))]
pub fn shutdown_host_filesystems() -> ax_errno::AxResult {
    host::arceos::shutdown_host_filesystems()
}

/// Read host monotonic time in nanoseconds.
#[cfg(target_arch = "x86_64")]
pub(crate) fn monotonic_time_nanos() -> u64 {
    host::arceos::monotonic_time_nanos()
}
