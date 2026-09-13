use ax_cpu::virtualization::Exception;
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
use ax_cpu::virtualization::{self as cpu, GuestPrivilege};

use crate::arch::riscv64::policy::types::{RiscvGuestPhysAddr, RiscvGuestVirtAddr};

/// Fault categories produced while fetching a guest instruction with HLVX.
///
/// HLVX checks execute permission, but architecturally reports load-class
/// exceptions. These variants carry the guest-facing fetch semantics that the
/// vCPU code should inject or forward.
#[derive(Debug)]
pub(crate) enum GuestInstructionFetchFault {
    /// Guest VS-stage translation denied or missed the instruction address.
    PageFault { addr: RiscvGuestVirtAddr },
    /// Guest instruction access fault after translation.
    AccessFault { addr: RiscvGuestVirtAddr },
    /// Guest instruction address was misaligned.
    Misaligned { addr: RiscvGuestVirtAddr },
    /// G-stage translation fault while resolving the instruction access.
    GuestPageFault { addr: RiscvGuestPhysAddr },
    /// A trap cause that is not expected from HLVX instruction fetching.
    Unhandled {
        scause: usize,
        stval: usize,
        htval: usize,
    },
}

fn into_fetch_fault(
    fault: ax_cpu::virtualization::GuestAccessFault,
    gva: RiscvGuestVirtAddr,
) -> GuestInstructionFetchFault {
    let exception = fault.scause & !(1usize << (usize::BITS - 1));
    let fault_gva = RiscvGuestVirtAddr::from_usize(fault.stval);

    match exception {
        // HLVX reports these as load faults even though the guest-visible
        // operation is an instruction fetch.
        x if x == Exception::InstructionPageFault as usize
            || x == Exception::LoadPageFault as usize =>
        {
            GuestInstructionFetchFault::PageFault { addr: fault_gva }
        }
        x if x == Exception::InstructionFault as usize || x == Exception::LoadFault as usize => {
            GuestInstructionFetchFault::AccessFault { addr: fault_gva }
        }
        x if x == Exception::InstructionMisaligned as usize
            || x == Exception::LoadMisaligned as usize =>
        {
            GuestInstructionFetchFault::Misaligned { addr: gva }
        }
        x if x == Exception::InstructionGuestPageFault as usize
            || x == Exception::LoadGuestPageFault as usize =>
        {
            // For guest-page faults, htval holds GPA[XLEN-1:2] and stval
            // supplies the low two bits of the faulting guest physical address.
            let fault_gpa = RiscvGuestPhysAddr::from_usize(
                ax_cpu::virtualization::guest_page_fault_addr(fault.htval, fault.stval),
            );
            GuestInstructionFetchFault::GuestPageFault { addr: fault_gpa }
        }
        _ => GuestInstructionFetchFault::Unhandled {
            scause: fault.scause,
            stval: fault.stval,
            htval: fault.htval,
        },
    }
}

pub(crate) fn copy_from_guest_va(
    dst: &mut [u8],
    gva: RiscvGuestVirtAddr,
    supervisor: bool,
) -> usize {
    // SAFETY: AxVM holds this vCPU's hardware binding and guest memory ownership
    // through the SBI copy; buffers are VMM-owned allocations outside guest RAM.
    unsafe {
        cpu::copy_from_guest_virtual(
            dst,
            gva.as_usize().into(),
            if supervisor {
                GuestPrivilege::Supervisor
            } else {
                GuestPrivilege::User
            },
        )
    }
}

pub(crate) fn copy_from_guest(dst: &mut [u8], gpa: RiscvGuestPhysAddr) -> usize {
    // SAFETY: AxVM holds this vCPU's hardware binding and guest memory ownership
    // through the SBI copy; buffers are VMM-owned allocations outside guest RAM.
    unsafe { cpu::copy_from_guest_physical(dst, gpa.as_usize().into()) }
}

pub(crate) fn copy_to_guest(src: &[u8], gpa: RiscvGuestPhysAddr) -> usize {
    // SAFETY: AxVM holds this vCPU's hardware binding and guest memory ownership
    // through the SBI copy; buffers are VMM-owned allocations outside guest RAM.
    unsafe { cpu::copy_to_guest_physical(src, gpa.as_usize().into()) }
}

pub(crate) fn fetch_guest_instruction(
    gva: RiscvGuestVirtAddr,
    supervisor: bool,
) -> Result<u32, GuestInstructionFetchFault> {
    let privilege = if supervisor {
        GuestPrivilege::Supervisor
    } else {
        GuestPrivilege::User
    };
    // SAFETY: the instruction emulator runs inside the pinned vCPU binding.
    unsafe { cpu::fetch_guest_instruction(gva.as_usize().into(), privilege) }
        .map_err(|fault| into_fetch_fault(fault, gva))
}
