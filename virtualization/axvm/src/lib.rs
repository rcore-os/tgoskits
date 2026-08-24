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
mod vmexit_stats;

#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "arch/aarch64/shared_mmio.rs"]
mod aarch64_shared_mmio_tests;
#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "arch/aarch64/vtimer/percpu.rs"]
mod aarch64_timer_percpu_tests;
#[cfg(all(test, not(target_arch = "aarch64")))]
#[path = "arch/aarch64/wfi.rs"]
mod aarch64_wfi_tests;

use crate::arch::ArchOps;

pub mod config;

pub use arch::platform::*;
pub use ax_cpumask::CpuMask;
pub use axdevice::{SerialBackend, SerialBackendFactory};
pub use axvm_types::{
    AccessWidth, GuestPhysAddr, HostPhysAddr, InterruptTriggerMode, MappingFlags, Port, SysRegAddr,
    VMId, VmVcpuState,
};
pub use config::Aarch64WfiPolicy;
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
    AxvmRuntime, PeriodicVirqConfig, current_vcpu_id, current_vm_id,
    dispatch_current_vcpu_interrupt, get_vm_by_id, get_vm_list, inject_current_vcpu_interrupt,
    notify_vm_vcpu, register_vm,
};
pub(crate) use task::{AsVCpuTask, VCpuTask};
pub use vm::{
    AxVM, AxVMRef, FwCfgDeviceConfig, PreparedMemoryLayout, VMMemoryRegion, VcpuSnapshot,
};
pub use vmexit_stats::{
    CpuExitCounts, ExitReason, MAX_TRACKED_CPUS, vmexit_stats_reset, vmexit_stats_snapshot,
};

/// Low-overhead vCPU wait counters used by post-stall diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VcpuRuntimeCounts {
    pub vcpu_id: usize,
    pub post_vmexit_yields: usize,
    pub parks: usize,
    pub wakes: usize,
    pub notify_woke: usize,
    pub vtimer_arms: usize,
    pub vtimer_immediate: usize,
    pub vtimer_no_deadline: usize,
    pub vtimer_registered: usize,
    pub vtimer_callbacks: usize,
    pub vtimer_stale_callbacks: usize,
    pub vtimer_notifications: usize,
    pub vtimer_invalidations: usize,
    pub vtimer_direct_acks: usize,
    pub vtimer_direct_overlaps: usize,
    pub vtimer_callback_to_wake_samples: usize,
    pub vtimer_callback_to_wake_overflow: usize,
    pub vtimer_callback_to_wake_p50_ns: u64,
    pub vtimer_callback_to_wake_p99_ns: u64,
    pub vtimer_callback_to_wake_p99_9_ns: u64,
    pub vtimer_callback_to_wake_max_ns: u64,
    /// Callback-to-`run_vcpu` dispatch latency, before architecture entry preparation.
    pub vtimer_callback_to_entry_samples: usize,
    pub vtimer_callback_to_entry_overflow: usize,
    pub vtimer_callback_to_entry_p50_ns: u64,
    pub vtimer_callback_to_entry_p99_ns: u64,
    pub vtimer_callback_to_entry_p99_9_ns: u64,
    pub vtimer_callback_to_entry_max_ns: u64,
    /// Callback-to-Guest backend entry latency, after VGIC load.
    pub vtimer_callback_to_guest_entry_samples: usize,
    pub vtimer_callback_to_guest_entry_overflow: usize,
    pub vtimer_callback_to_guest_entry_p50_ns: u64,
    pub vtimer_callback_to_guest_entry_p99_ns: u64,
    pub vtimer_callback_to_guest_entry_p99_9_ns: u64,
    pub vtimer_callback_to_guest_entry_max_ns: u64,
    /// Direct CNTV acknowledge/publish completion to the next vCPU run dispatch.
    pub vtimer_direct_to_entry_samples: usize,
    pub vtimer_direct_to_entry_overflow: usize,
    pub vtimer_direct_to_entry_p50_ns: u64,
    pub vtimer_direct_to_entry_p99_ns: u64,
    pub vtimer_direct_to_entry_p99_9_ns: u64,
    pub vtimer_direct_to_entry_max_ns: u64,
    /// Direct CNTV acknowledge/publish completion to Guest backend entry.
    pub vtimer_direct_to_guest_entry_samples: usize,
    pub vtimer_direct_to_guest_entry_overflow: usize,
    pub vtimer_direct_to_guest_entry_p50_ns: u64,
    pub vtimer_direct_to_guest_entry_p99_ns: u64,
    pub vtimer_direct_to_guest_entry_p99_9_ns: u64,
    pub vtimer_direct_to_guest_entry_max_ns: u64,
    /// Host CNTV PPI activation lifetime from acceptance until guest EOI.
    pub vtimer_activation_hold_samples: usize,
    pub vtimer_activation_hold_overflow: usize,
    pub vtimer_activation_hold_p50_ns: u64,
    pub vtimer_activation_hold_p99_ns: u64,
    pub vtimer_activation_hold_p99_9_ns: u64,
    pub vtimer_activation_hold_max_ns: u64,
}

/// Low-overhead AxVM timer-wheel counters used by post-stall diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerRuntimeCounts {
    pub cpu_id: usize,
    pub snapshot_now_ns: u64,
    pub wheel_next_deadline_ns: u64,
    pub published_deadline_ns: u64,
    pub registered: usize,
    pub cancelled: usize,
    pub expired: usize,
    pub worker_wakes: usize,
    pub expiry_batches: usize,
    pub expiry_late_samples: usize,
    pub expiry_late_overflow: usize,
    pub expiry_late_p50_ns: u64,
    pub expiry_late_p99_ns: u64,
    pub expiry_late_p99_9_ns: u64,
    pub expiry_late_max_ns: u64,
    pub lock_acquisitions: usize,
    pub lock_wait_total_ns: u64,
    pub lock_wait_max_ns: u64,
    pub lock_hold_total_ns: u64,
    pub lock_hold_max_ns: u64,
}

/// Result of a concurrent AxVM software-timer register/cancel/expiry run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TimerStormResult {
    pub implementation: &'static str,
    pub cpu_mask: usize,
    pub workers: usize,
    pub iterations_per_worker: usize,
    pub register_cancel_pairs: usize,
    pub elapsed_ns: u64,
    pub pairs_per_second: u64,
    pub expiry_samples: usize,
    pub expiry_completed: usize,
    pub expiry_p50_late_ns: u64,
    pub expiry_p99_late_ns: u64,
    pub expiry_max_late_ns: u64,
    pub lock_acquisitions: usize,
    pub lock_wait_total_ns: u64,
    pub lock_wait_max_ns: u64,
    pub lock_hold_total_ns: u64,
    pub lock_hold_max_ns: u64,
}

/// Per-VM device-poll request counters used by console stall diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DevicePollRuntimeCounts {
    pub published: usize,
    pub kicked: usize,
    pub consumed: usize,
    pub pending: bool,
}

/// Snapshot of counters whose deltas distinguish vCPU, timer, and wake stalls.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RtRuntimeStats {
    pub vcpus: Vec<VcpuRuntimeCounts>,
    pub timers: Vec<TimerRuntimeCounts>,
    pub lr_skips: usize,
}

pub fn rt_runtime_stats_snapshot() -> RtRuntimeStats {
    RtRuntimeStats {
        vcpus: runtime::vcpus::rt_vcpu_stats_snapshot(),
        timers: timer::rt_timer_stats_snapshot(),
        lr_skips: runtime::vcpus::LR_SKIP_COUNT.load(core::sync::atomic::Ordering::Relaxed),
    }
}

/// The architecture-independent per-CPU type.
pub(crate) type AxVMPerCpu = vcpu::AxPerCpu<arch::ArchPerCpu>;

/// Check and dispatch pending AxVM timer events on the current CPU.
pub fn check_timer_events() {
    timer::check_events();
}

/// Run a bounded, multi-CPU software-timer storm for timer-wheel A/B testing.
#[cfg(not(test))]
pub fn run_timer_storm(
    cpu_mask: usize,
    iterations_per_worker: usize,
    expiry_samples_per_worker: usize,
    expiry_delay: core::time::Duration,
) -> Result<TimerStormResult, &'static str> {
    timer::run_timer_storm(
        cpu_mask,
        iterations_per_worker,
        expiry_samples_per_worker,
        expiry_delay,
    )
}

/// Clean data cache lines covering a host virtual address range.
pub fn clean_dcache_range(addr: ax_memory_addr::VirtAddr, size: usize) {
    arch::CurrentArch::clean_dcache_range(addr, size);
}
