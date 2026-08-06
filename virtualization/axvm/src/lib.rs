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

//! This crate provides a minimal VM monitor (VMM) for running guest VMs.
//!
//! This crate contains:
//! - [`AxVM`]: The main structure representing a VM.

#![cfg_attr(any(test, target_arch = "aarch64"), feature(once_cell_try))]

#[macro_use]
extern crate log;

mod arch;
mod architecture;
pub mod boot;
mod configured;
mod error;
pub mod host;
pub mod irq;
pub mod layout;
pub mod lifecycle;
pub mod machine;
mod manager;
mod npt;
mod percpu;
mod runtime;
mod sync;
mod task;
mod timer;
mod vcpu;
mod vm;

#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "arch/aarch64/shared_mmio.rs"]
mod aarch64_shared_mmio_tests;
#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "arch/aarch64/vtimer/percpu.rs"]
mod aarch64_timer_percpu_tests;

use crate::arch::ArchOps;

pub mod config;

pub use arch::platform::*;
pub use ax_cpumask::CpuMask;
pub use axdevice::{SerialBackend, SerialBackendFactory};
pub use axvm_types::{
    AccessWidth, GuestPhysAddr, HostPhysAddr, InterruptTriggerMode, MappingFlags, Port, SysRegAddr,
    VMId, VmVcpuState,
};
pub use configured::{
    ConfiguredDeviceCatalog, ConfiguredDeviceError, ConfiguredModelConstructor,
    ConfiguredModelRegistration, DefaultVirtualDeviceIntent, DeviceInstantiationContext,
    FixedDeviceBindings, FixedWiredBinding,
};
pub use error::{AxVmError, AxVmResult};
pub(crate) use error::{ax_err, ax_err_type};
pub(crate) use host::{
    paging::HostPagingHandler,
    task::{AxTaskExt, AxTaskRef, TaskInner, WaitQueue, WaitQueueHandle as HostWaitQueueHandle},
};
pub use lifecycle::{StopReason, VmStatus};
pub use manager::{
    AxvmRuntime, current_vcpu_id, current_vm_id, dispatch_current_vcpu_interrupt, get_vm_by_id,
    get_vm_list, inject_current_vcpu_interrupt, notify_vm_vcpu, register_vm,
};
pub(crate) use task::{AsVCpuTask, VCpuTask};
pub use vm::{
    AxVM, AxVMRef, FwCfgDeviceConfig, PreparedMemoryLayout, VMMemoryRegion, VcpuSnapshot,
};

/// The architecture-independent per-CPU type.
pub(crate) type AxVMPerCpu = vcpu::AxPerCpu<arch::ArchPerCpu>;

/// Check and dispatch pending AxVM timer events on the current CPU.
pub fn check_timer_events() {
    timer::check_events();
}

/// Clean data cache lines covering a host virtual address range.
pub fn clean_dcache_range(addr: ax_memory_addr::VirtAddr, size: usize) {
    arch::CurrentArch::clean_dcache_range(addr, size);
}
