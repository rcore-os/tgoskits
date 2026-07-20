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

use core::arch::riscv64::hfence_vvma_all;

use riscv_h::register::vsatp::Vsatp;

use crate::{
    registers::guest_page_fault_addr,
    trap::Exception,
    types::{RiscvGuestPhysAddr, RiscvGuestVirtAddr},
};

// Notes about this file:
//
// 1. This file ("mem_extable.S") comes from salus from Rivos Inc. A copy of the original file can
//    be found at https://github.com/rivosinc/salus/blob/27b8d1dbeec96cbcac929c7875a21ec0af03d1bb/src/mem_extable.S
// 2. The original file contains mechanisms to handle exceptions during guest memory accesses, which
//    is not used for now in our implementation. However, it's possible for us to implement these
//    in the future, so we keep the original file here.
// 3. The original file mistakenly declared that `_copy_from_guest` and `_copy_to_guest` are copying
//    from/to guest physical addresses, but they are actually copying from/to guest virtual
//    addresses (as hlv/hsv instructions do). We fix description here.
core::arch::global_asm!(include_str!("mem_extable.S"));

unsafe extern "C" {
    /// Copy data from guest virtual address to host address.
    fn _copy_from_guest(dst: *mut u8, src_gva: usize, len: usize) -> usize;
    /// Copy data from host address to guest virtual address.
    fn _copy_to_guest(dst_gva: usize, src: *const u8, len: usize) -> usize;
    /// Fetch the guest instruction at the given guest virtual address.
    ///
    /// Returns zero on success. On fault, returns non-zero and writes the trap
    /// CSRs captured at the HLVX fault site into `fault`.
    fn _fetch_guest_instruction(
        gva: usize,
        raw_inst: *mut u32,
        fault: *mut GuestInstructionFetchFaultRaw,
    ) -> usize;
}

#[derive(Debug, Default)]
#[repr(C)]
/// Raw trap CSR snapshot returned by the HLVX fetch helper on fault.
struct GuestInstructionFetchFaultRaw {
    scause: usize,
    stval: usize,
    htval: usize,
}

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

impl GuestInstructionFetchFaultRaw {
    fn into_fetch_fault(self, gva: RiscvGuestVirtAddr) -> GuestInstructionFetchFault {
        let exception = self.scause & !(1usize << (usize::BITS - 1));
        let fault_gva = RiscvGuestVirtAddr::from_usize(self.stval);

        match exception {
            // HLVX reports these as load faults even though the guest-visible
            // operation is an instruction fetch.
            x if x == Exception::InstructionPageFault as usize
                || x == Exception::LoadPageFault as usize =>
            {
                GuestInstructionFetchFault::PageFault { addr: fault_gva }
            }
            x if x == Exception::InstructionFault as usize
                || x == Exception::LoadFault as usize =>
            {
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
                let fault_gpa = guest_page_fault_addr(self.htval, self.stval);
                GuestInstructionFetchFault::GuestPageFault { addr: fault_gpa }
            }
            _ => GuestInstructionFetchFault::Unhandled {
                scause: self.scause,
                stval: self.stval,
                htval: self.htval,
            },
        }
    }
}

/// Copies data from guest virtual address to host memory.
#[inline(always)]
pub(crate) fn copy_from_guest_va(dst: &mut [u8], gva: RiscvGuestVirtAddr) -> usize {
    unsafe { _copy_from_guest(dst.as_mut_ptr(), gva.as_usize(), dst.len()) }
}

/// Copies data from host memory to guest virtual address.
#[inline(always)]
pub(crate) fn copy_to_guest_va(src: &[u8], gva: RiscvGuestVirtAddr) -> usize {
    unsafe { _copy_to_guest(gva.as_usize(), src.as_ptr(), src.len()) }
}

/// Copies data from guest physical address to host memory.
#[inline(always)]
pub(crate) fn copy_from_guest(dst: &mut [u8], gpa: RiscvGuestPhysAddr) -> usize {
    let old_vsatp = riscv_h::register::vsatp::read().bits();
    unsafe {
        // Set vsatp to 0 to disable guest virtual address translation.
        Vsatp::from_bits(0).write();
        hfence_vvma_all();
        // Now GVA is the same as GPA.
        let ret = copy_from_guest_va(dst, RiscvGuestVirtAddr::from(gpa.as_usize()));
        // Restore the original vsatp.
        Vsatp::from_bits(old_vsatp).write();
        hfence_vvma_all();
        ret
    }
}

///  Copies data from host memory to guest physical address.
#[inline(always)]
pub(crate) fn copy_to_guest(src: &[u8], gpa: RiscvGuestPhysAddr) -> usize {
    let old_vsatp = riscv_h::register::vsatp::read().bits();
    unsafe {
        // Set vsatp to 0 to disable guest virtual address translation.
        Vsatp::from_bits(0).write();
        hfence_vvma_all();
        // Now GVA is the same as GPA.
        let ret = copy_to_guest_va(src, RiscvGuestVirtAddr::from_usize(gpa.as_usize()));
        // Restore the original vsatp.
        Vsatp::from_bits(old_vsatp).write();
        hfence_vvma_all();
        ret
    }
}

/// Fetches the guest instruction at the given guest virtual address.
///
/// The assembly helper uses HLVX so execute permission is checked as a guest
/// instruction fetch, while this wrapper converts the load-class trap CSRs back
/// into guest instruction-fetch categories.
#[inline(always)]
pub(crate) fn fetch_guest_instruction(
    gva: RiscvGuestVirtAddr,
) -> Result<u32, GuestInstructionFetchFault> {
    let mut inst = 0u32;
    let mut fault = GuestInstructionFetchFaultRaw::default();
    let ret = unsafe {
        _fetch_guest_instruction(
            gva.as_usize(),
            &mut inst as *mut u32,
            &mut fault as *mut GuestInstructionFetchFaultRaw,
        )
    };
    if ret == 0 {
        Ok(inst)
    } else {
        Err(fault.into_fetch_fault(gva))
    }
}
