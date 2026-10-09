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
//! - [`VmManager`]: VM instance creation and registry ownership.
//! - [`VmHandle`]: lifecycle requests and immutable observations.

#![cfg_attr(any(test, target_arch = "aarch64"), feature(once_cell_try))]

#[macro_use]
extern crate log;

mod arch;
mod architecture;
pub mod boot;
mod configured;
mod control;
mod engine;
mod error;
mod guest_memory;
pub mod host;
mod identity;
pub mod irq;
pub mod layout;
pub mod lifecycle;
pub mod machine;
mod manager;
mod npt;
mod operation;
mod percpu;
mod runtime;
mod services;
mod sync;
mod task;
mod vcpu;
mod vm;

pub mod config;

pub use ax_cpumask::CpuMask;
pub use ax_std::os::arceos::task::sched::{RtPriority, SchedulePolicy};
pub use axdevice::{SerialBackend, SerialBackendFactory};
pub use axvm_types::{
    AccessWidth, GuestPhysAddr, HostPhysAddr, InterruptTriggerMode, MappingFlags, Port, SysRegAddr,
    VMId, VmBackendError, VmVcpuState,
};
pub use configured::{
    ConfiguredDeviceCatalog, ConfiguredDeviceError, ConfiguredModelConstructor,
    ConfiguredModelRegistration, DefaultVirtualDeviceIntent, DeviceInstantiationContext,
    FixedDeviceBindings, FixedWiredBinding, PhysicalUplink, PhysicalUplinkError, VirtioPciFunction,
    install_physical_uplink, physical_uplink_installed, reserve_host_mac, switch_from_physical_rx,
    virtio_capabilities,
};
pub use error::{AxVmError, AxVmResult};
pub(crate) use error::{ax_err, ax_err_type};
pub use guest_memory::{GuestMemoryPort, GuestRange, MappingLease, MemoryRevision, MemoryUpdate};
pub(crate) use host::{
    paging::HostPagingHandler,
    task::{ThreadHandle, WaitQueueHandle as HostWaitQueueHandle},
};
pub use identity::{OperationId, RunId, VmKey};
pub use lifecycle::{StopReason, VmStatus};
pub use manager::{
    CpuObservation, DeviceObservation, MemoryObservation, VmConfigSnapshot, VmCreatePlan, VmHandle,
    VmManager, VmSnapshot,
};
pub use operation::VmOperation;
pub use runtime::queue::SignalError;
pub use services::VcpuInterruptPort;
pub(crate) use vm::AxVM;
pub use vm::{FwCfgDeviceConfig, PreparedMemoryLayout, VMMemoryRegion, VcpuSnapshot};

/// Returns the guest identity currently loaded on this CPU.
pub fn current_vm_id() -> Option<VMId> {
    vcpu::with_current_execution(|current| current.map(|context| context.vm_id()))
}

/// Returns the virtual CPU currently loaded on this CPU.
pub fn current_vcpu_id() -> Option<usize> {
    vcpu::with_current_execution(|current| current.map(|context| context.vcpu_id()))
}

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
