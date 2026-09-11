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

//! Guest virtual/physical memory access through HLV, HSV and HLVX.

use core::{arch::asm, mem::offset_of};

use crate::virtualization::{GuestPhysAddr, GuestVirtAddr};

/// Machine exception recorded by an HLVX instruction fetch.
#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct GuestAccessFault {
    /// Raw supervisor trap cause, retaining unknown exception encodings.
    pub scause: usize,
    /// Faulting guest virtual address supplied by the hardware.
    pub stval: usize,
    /// G-stage fault address shifted right by two, when supplied by hardware.
    pub htval: usize,
}

/// Guest privilege used for explicit hypervisor memory accesses.
#[derive(Debug, Clone, Copy)]
pub enum GuestPrivilege {
    /// Access as a virtual user.
    User,
    /// Access as a virtual supervisor.
    Supervisor,
}

core::arch::global_asm!(include_str!("../entry/guest_memory.S"),
    fault_scause = const offset_of!(GuestAccessFault, scause),
    fault_stval = const offset_of!(GuestAccessFault, stval),
    fault_htval = const offset_of!(GuestAccessFault, htval),
);

unsafe extern "C" {
    fn __ax_cpu_copy_from_guest(dst: *mut u8, gva: usize, length: usize) -> usize;
    fn __ax_cpu_copy_to_guest(gva: usize, src: *const u8, length: usize) -> usize;
    fn __ax_cpu_fetch_guest_instruction(
        gva: usize,
        instruction: *mut u32,
        fault: *mut GuestAccessFault,
    ) -> usize;
}

/// Executes a nofault read from the current guest virtual address space.
/// Returns the number of bytes copied before success or an access fault.
///
/// # Safety
/// The caller owns the installed guest translation and retains a CPU pin.
/// Source guest mappings must not alias the destination's exclusive borrow.
/// The normal CPU trap vector and initialized exception tables must be active.
pub unsafe fn copy_from_guest_virtual(
    dst: &mut [u8],
    address: GuestVirtAddr,
    privilege: GuestPrivilege,
) -> usize {
    // SAFETY: the caller owns the active translation and its hardware bank.
    let _access = unsafe { AccessScope::new(privilege, false) };
    // SAFETY: the destination is a live exclusive slice; guest faults use fixups.
    unsafe { __ax_cpu_copy_from_guest(dst.as_mut_ptr(), address.as_usize(), dst.len()) }
}

/// Executes a nofault write to the current guest virtual address space.
/// Returns the number of bytes copied before success or an access fault.
///
/// # Safety
/// The caller owns the guest destination, installed translation and CPU pin.
/// Guest mappings must not alias the source borrow or other live host objects.
/// The normal CPU trap vector and initialized exception tables must be active.
pub unsafe fn copy_to_guest_virtual(
    src: &[u8],
    address: GuestVirtAddr,
    privilege: GuestPrivilege,
) -> usize {
    // SAFETY: the caller owns the active translation and its hardware bank.
    let _access = unsafe { AccessScope::new(privilege, false) };
    // SAFETY: source is a live slice; the caller owns the guest destination.
    unsafe { __ax_cpu_copy_to_guest(address.as_usize(), src.as_ptr(), src.len()) }
}

/// Reads guest physical memory using the installed G-stage translation.
/// Returns the number of bytes copied before success or an access fault.
///
/// # Safety
/// The caller owns G-stage translation and the CPU pin. Source mappings must
/// not alias `dst`; normal CPU traps and exception tables must be initialized.
pub unsafe fn copy_from_guest_physical(dst: &mut [u8], address: GuestPhysAddr) -> usize {
    // SAFETY: the caller owns both stages through restoration of VSATP.
    let _access = unsafe { AccessScope::new(GuestPrivilege::Supervisor, true) };
    // SAFETY: VSATP is temporarily Bare; all G-stage faults use fixups.
    unsafe { __ax_cpu_copy_from_guest(dst.as_mut_ptr(), address.as_usize(), dst.len()) }
}

/// Writes guest physical memory using the installed G-stage translation.
/// Returns the number of bytes copied before success or an access fault.
///
/// # Safety
/// The caller owns G-stage translation, the destination and the CPU pin.
/// The destination must not alias live host objects or `src`; normal CPU traps
/// and exception tables must be initialized.
pub unsafe fn copy_to_guest_physical(src: &[u8], address: GuestPhysAddr) -> usize {
    // SAFETY: the caller owns both stages through restoration of VSATP.
    let _access = unsafe { AccessScope::new(GuestPrivilege::Supervisor, true) };
    // SAFETY: VSATP is Bare and the caller owns the guest destination.
    unsafe { __ax_cpu_copy_to_guest(address.as_usize(), src.as_ptr(), src.len()) }
}

/// Fetches a two- or four-byte guest instruction using execute permission.
/// Reports raw machine fault state; guest exception injection belongs to the VMM.
///
/// # Safety
/// The caller owns the installed guest translation and retains the CPU pin.
/// Normal CPU traps and initialized exception tables must be active.
pub unsafe fn fetch_guest_instruction(
    address: GuestVirtAddr,
    privilege: GuestPrivilege,
) -> Result<u32, GuestAccessFault> {
    // SAFETY: caller owns the translation through the bounded fetch operation.
    let _access = unsafe { AccessScope::new(privilege, false) };
    let mut instruction = 0;
    let mut fault = GuestAccessFault::default();
    // SAFETY: outputs are exclusive initialized objects and fixups bound the fetch.
    let result = unsafe {
        __ax_cpu_fetch_guest_instruction(address.as_usize(), &mut instruction, &mut fault)
    };
    if result == 0 {
        Ok(instruction)
    } else {
        Err(fault)
    }
}

struct AccessScope {
    host_hstatus: usize,
    host_vsatp: Option<usize>,
    irq: bool,
}

impl AccessScope {
    unsafe fn new(privilege: GuestPrivilege, physical: bool) -> Self {
        let irq = crate::interrupt::irqs_enabled();
        crate::interrupt::disable_irqs();
        let host_hstatus: usize;
        // SAFETY: caller owns H state and IRQ exclusion prevents reentry.
        unsafe {
            asm!("csrr {}, hstatus", out(reg) host_hstatus, options(nostack));
        }
        let guest_hstatus = match privilege {
            GuestPrivilege::User => host_hstatus & !(1 << 8),
            GuestPrivilege::Supervisor => host_hstatus | (1 << 8),
        };
        // SAFETY: only SPVP changes, and the original value is retained.
        unsafe {
            asm!("csrw hstatus, {}", in(reg) guest_hstatus, options(nostack));
        }
        let host_vsatp = if physical {
            let saved;
            // SAFETY: IRQs stay disabled through restoring the borrowed VS bank.
            unsafe {
                asm!("csrrw {}, vsatp, zero", out(reg) saved, options(nostack));
                fence_vs();
            }
            Some(saved)
        } else {
            None
        };
        Self {
            host_hstatus,
            host_vsatp,
            irq,
        }
    }
}

impl Drop for AccessScope {
    fn drop(&mut self) {
        if let Some(vsatp) = self.host_vsatp {
            // SAFETY: this scope still exclusively owns the IRQ-disabled VS bank.
            unsafe {
                asm!("csrw vsatp, {}", in(reg) vsatp, options(nostack));
                fence_vs();
            }
        }
        // SAFETY: restore host privilege interpretation before opening IRQs.
        unsafe {
            asm!("csrw hstatus, {}", in(reg) self.host_hstatus, options(nostack));
        }
        if self.irq {
            crate::interrupt::enable_irqs();
        }
    }
}

unsafe fn fence_vs() {
    // SAFETY: caller owns the current HS/VS translation state.
    unsafe {
        asm!(
            ".option push",
            ".option arch, +h",
            "hfence.vvma",
            ".option pop",
            options(nostack)
        );
    }
}
