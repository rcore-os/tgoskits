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

//! Runtime state for the KVM-compatible control endpoint.
//!
//! Pure ioctl payloads are imported from `kvm-uapi`. The types defined here own
//! AxVisor and host-control resources, so they intentionally remain in
//! axvisor_core.

use alloc::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::AtomicBool;

use axaddrspace::device::AccessWidth;
use axvisor_api::{control as api_control, task::TaskHandle};
use axvm::AxVMRef;
#[cfg(target_arch = "x86_64")]
pub(in crate::kvm) use kvm_uapi::x86::{PvClockVcpuTimeInfo, PvClockWallClock};
pub(in crate::kvm) use kvm_uapi::{
    KvmCpuidEntry2, KvmEnableCap, KvmIoEventFd, KvmIrqFd, OneReg, UserspaceMemoryRegion,
};

#[derive(Clone)]
pub(in crate::kvm) enum ControlFileState {
    System,
    Vm(VmFileState),
    Vcpu(VcpuFileState),
}

#[derive(Clone)]
pub(in crate::kvm) struct VmFileState {
    pub(in crate::kvm) vm: AxVMRef,
    pub(in crate::kvm) memory_slots: BTreeMap<u32, MemorySlot>,
    pub(in crate::kvm) ioeventfds: BTreeMap<IoEventFdKey, IoEventFd>,
    pub(in crate::kvm) irqfds: BTreeMap<IrqFdKey, IrqFd>,
    pub(in crate::kvm) gsi_routes: BTreeMap<u32, GsiRoute>,
    pub(in crate::kvm) vcpu_files: BTreeMap<u32, api_control::ControlFileId>,
    pub(in crate::kvm) clock: Vec<u8>,
    pub(in crate::kvm) pit2: Vec<u8>,
    pub(in crate::kvm) tsc_khz: u32,
    pub(in crate::kvm) tss_addr: Option<usize>,
    pub(in crate::kvm) irqchip_created: bool,
    pub(in crate::kvm) pit2_created: bool,
    pub(in crate::kvm) gsi_routing_count: u32,
}

/// Host-side state associated with one KVM vCPU fd.
///
/// Some fields, such as FPU/LAPIC/XSAVE blobs, are stored as opaque bytes
/// because the current control endpoint only needs to preserve KVM ABI payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct VcpuFileState {
    pub(in crate::kvm) vm_file: api_control::ControlFileId,
    pub(in crate::kvm) vcpu_id: u32,
    pub(in crate::kvm) mmap_area: api_control::MmapAreaId,
    pub(in crate::kvm) mp_state: u32,
    pub(in crate::kvm) pending_interrupts: VecDeque<usize>,
    pub(in crate::kvm) pending_mmio_read: Option<PendingMmioRead>,
    pub(in crate::kvm) pending_io_read: Option<PendingIoRead>,
    pub(in crate::kvm) cpuid: Vec<KvmCpuidEntry2>,
    pub(in crate::kvm) msrs: BTreeMap<u32, u64>,
    pub(in crate::kvm) fpu: Vec<u8>,
    pub(in crate::kvm) vcpu_events: Vec<u8>,
    pub(in crate::kvm) debugregs: Vec<u8>,
    pub(in crate::kvm) xsave: Vec<u8>,
    pub(in crate::kvm) xcrs: Vec<u8>,
    pub(in crate::kvm) signal_mask: Vec<u8>,
    pub(in crate::kvm) lapic: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct PendingMmioRead {
    pub(in crate::kvm) reg: usize,
    pub(in crate::kvm) width: AccessWidth,
    pub(in crate::kvm) reg_width: AccessWidth,
    pub(in crate::kvm) signed_ext: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct PendingIoRead {
    pub(in crate::kvm) width: AccessWidth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct MemorySlot {
    pub(in crate::kvm) flags: u32,
    pub(in crate::kvm) guest_phys_addr: u64,
    pub(in crate::kvm) memory_size: u64,
    pub(in crate::kvm) userspace_addr: u64,
    pub(in crate::kvm) pinned_pages: api_control::PinnedUserPagesId,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::kvm) struct IoEventFdKey {
    pub(in crate::kvm) addr: u64,
    pub(in crate::kvm) datamatch: u64,
    pub(in crate::kvm) pio: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct IoEventFd {
    pub(in crate::kvm) addr: u64,
    pub(in crate::kvm) len: u32,
    pub(in crate::kvm) datamatch: u64,
    pub(in crate::kvm) user_fd_ref: api_control::UserFdRefId,
    pub(in crate::kvm) flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::kvm) struct IrqFdKey {
    pub(in crate::kvm) gsi: u32,
    pub(in crate::kvm) fd: u32,
}

#[derive(Clone, Debug)]
pub(in crate::kvm) struct IrqFd {
    pub(in crate::kvm) user_fd_ref: api_control::UserFdRefId,
    pub(in crate::kvm) cancel: Arc<AtomicBool>,
    pub(in crate::kvm) _task: TaskHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) enum GsiRoute {
    IrqChip { pin: u32 },
    Msi { vector: u8 },
}
