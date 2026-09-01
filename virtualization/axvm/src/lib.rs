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
mod vcpu;
mod vm;

pub mod config;

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
pub(crate) type AxVMPerCpu = vcpu::AxPerCpu<arch::current::ArchPerCpu>;

#[cfg(test)]
mod host_link_symbols {
    #[unsafe(no_mangle)]
    static STACK_SIZE: usize = 0;
    #[unsafe(no_mangle)]
    static PAGE_SIZE: usize = 0;
    #[unsafe(no_mangle)]
    static __PERCPU_TEMPLATE_ALIGN_START: usize = 0;
    #[unsafe(no_mangle)]
    static __PERCPU_TEMPLATE_ALIGN_END: usize = 0;
}
