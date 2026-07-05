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

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::AtomicBool;

use axaddrspace::device::AccessWidth;
use axvisor_api::{control as api_control, task::TaskHandle};
use axvm::AxVMRef;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct VcpuFileState {
    pub(in crate::kvm) vm_file: api_control::ControlFileId,
    pub(in crate::kvm) vcpu_id: u32,
    pub(in crate::kvm) mmap_area: api_control::MmapAreaId,
    pub(in crate::kvm) mp_state: u32,
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

#[cfg(target_arch = "x86_64")]
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::kvm) struct PvClockWallClock {
    pub(in crate::kvm) version: u32,
    pub(in crate::kvm) sec: u32,
    pub(in crate::kvm) nsec: u32,
}

#[cfg(target_arch = "x86_64")]
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::kvm) struct PvClockVcpuTimeInfo {
    pub(in crate::kvm) version: u32,
    pub(in crate::kvm) pad0: u32,
    pub(in crate::kvm) tsc_timestamp: u64,
    pub(in crate::kvm) system_time: u64,
    pub(in crate::kvm) tsc_to_system_mul: u32,
    pub(in crate::kvm) tsc_shift: i8,
    pub(in crate::kvm) flags: u8,
    pub(in crate::kvm) pad: [u8; 2],
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct UserspaceMemoryRegion {
    pub(in crate::kvm) slot: u32,
    pub(in crate::kvm) flags: u32,
    pub(in crate::kvm) guest_phys_addr: u64,
    pub(in crate::kvm) memory_size: u64,
    pub(in crate::kvm) userspace_addr: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct OneReg {
    pub(in crate::kvm) id: u64,
    pub(in crate::kvm) addr: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct KvmIoEventFd {
    pub(in crate::kvm) datamatch: u64,
    pub(in crate::kvm) addr: u64,
    pub(in crate::kvm) len: u32,
    pub(in crate::kvm) fd: i32,
    pub(in crate::kvm) flags: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::kvm) struct KvmCpuidEntry2 {
    pub(in crate::kvm) function: u32,
    pub(in crate::kvm) index: u32,
    pub(in crate::kvm) flags: u32,
    pub(in crate::kvm) eax: u32,
    pub(in crate::kvm) ebx: u32,
    pub(in crate::kvm) ecx: u32,
    pub(in crate::kvm) edx: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct KvmIrqFd {
    pub(in crate::kvm) fd: u32,
    pub(in crate::kvm) gsi: u32,
    pub(in crate::kvm) flags: u32,
    pub(in crate::kvm) resamplefd: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::kvm) struct KvmEnableCap {
    pub(in crate::kvm) cap: u32,
    pub(in crate::kvm) flags: u32,
    pub(in crate::kvm) args: [u64; 4],
}
