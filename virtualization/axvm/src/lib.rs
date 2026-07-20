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
mod architecture;
pub mod boot;
mod current_vcpu;
mod error;
mod host;
pub mod irq;
pub mod layout;
pub mod lifecycle;
mod manager;
mod npt;
mod percpu;
mod runtime;
mod task;
mod timer;
mod vcpu;
mod vm;

use crate::arch::ArchOps;

pub mod config;

pub use arch::platform::*;
pub use ax_cpumask::CpuMask;
/// Compatibility export for legacy/common normalized VM events.
///
/// Architecture-local raw exits are handled by `arch::CurrentArch` through
/// `VmArchVcpuOps::Exit`; new code should not treat this as the universal raw
/// vCPU exit type.
pub use axvm_types::VmExit;
pub use axvm_types::{
    AccessWidth, GuestPhysAddr, HostPhysAddr, InterruptTriggerMode, MappingFlags, Port, SysRegAddr,
    VMId, VmVcpuState,
};
pub use error::{AxVmError, AxVmResult};
pub(crate) use error::{ax_err, ax_err_type};
pub use host::irq_routes::{
    GuestIrqRouteError, GuestIrqRouteLease, GuestIrqRouteLeaseState, GuestIrqRoutesRevoked,
    activate_guest_irq_routes, revoke_guest_irq_route_lease,
};
#[cfg(any(feature = "fs", feature = "host-fs"))]
pub use host::storage::{
    GuestStorageRoutesRevoked, HostStorageHandoff, HostStorageHandoffError,
    HostStorageHandoffState, HostStoragePciEndpoint,
};
pub(crate) use host::{
    paging::HostPagingHandler,
    task::{AxTaskRef, TaskInner, WaitQueue, WaitQueueHandle as HostWaitQueueHandle},
};
pub use irq::InterruptFabric;
pub use lifecycle::{StopReason, VmStatus};
pub use manager::{
    AxvmRuntime, current_vcpu_id, current_vm_id, get_vm_by_id, get_vm_list,
    inject_current_vcpu_interrupt, inject_vm_vcpu_interrupt, register_vm,
};
pub use runtime::{
    DefaultVmRunReport, DefaultVmStartFailure, vcpus::vcpu_on as boot_secondary_vcpu,
};
pub(crate) use task::{AsVCpuTask, VCpuTask};
pub use timer::{cancel_timer as cancel_vm_timer, register_timer as register_vm_timer};
pub use vm::{
    AxVM, AxVMRef, FwCfgDeviceConfig, PreparedMemoryLayout, VMMemoryRegion, VcpuSnapshot,
};

/// The architecture-independent per-CPU type.
pub(crate) type AxVMPerCpu = vcpu::AxPerCpu<arch::ArchPerCpu>;

/// Clean data cache lines covering a host virtual address range.
pub fn clean_dcache_range(addr: ax_memory_addr::VirtAddr, size: usize) {
    arch::CurrentArch::clean_dcache_range(addr, size);
}
