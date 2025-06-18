use axaddrspace::{GuestPhysAddr, GuestVirtAddr};
use core::arch::riscv64::hfence_vvma_all;
use riscv::register::vsatp::Vsatp;

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
    fn _fetch_guest_instruction(gva: usize, raw_inst: *mut u32) -> isize;
}

/// Copies data from guest virtual address to host memory.
#[inline(always)]
pub(crate) fn copy_from_guest_va(dst: &mut [u8], gva: GuestVirtAddr) -> usize {
    unsafe { _copy_from_guest(dst.as_mut_ptr(), gva.as_usize(), dst.len()) }
}

/// Copies data from host memory to guest virtual address.
#[inline(always)]
pub(crate) fn copy_to_guest_va(src: &[u8], gva: GuestVirtAddr) -> usize {
    unsafe { _copy_to_guest(gva.as_usize(), src.as_ptr(), src.len()) }
}

/// Copies data from guest physical address to host memory.
#[inline(always)]
pub(crate) fn copy_from_guest(dst: &mut [u8], gpa: GuestPhysAddr) -> usize {
    let old_vsatp = riscv::register::vsatp::read().bits();
    unsafe {
        // Set vsatp to 0 to disable guest virtual address translation.
        Vsatp::from_bits(0).write();
        hfence_vvma_all();
        // Now GVA is the same as GPA.
        let ret = copy_from_guest_va(dst, GuestVirtAddr::from(gpa.as_usize()));
        // Restore the original vsatp.
        Vsatp::from_bits(old_vsatp).write();
        hfence_vvma_all();
        ret
    }
}

///  Copies data from host memory to guest physical address.
#[inline(always)]
pub(crate) fn copy_to_guest(src: &[u8], gpa: GuestPhysAddr) -> usize {
    let old_vsatp = riscv::register::vsatp::read().bits();
    unsafe {
        // Set vsatp to 0 to disable guest virtual address translation.
        Vsatp::from_bits(0).write();
        hfence_vvma_all();
        // Now GVA is the same as GPA.
        let ret = copy_to_guest_va(src, GuestVirtAddr::from_usize(gpa.as_usize()));
        // Restore the original vsatp.
        Vsatp::from_bits(old_vsatp).write();
        hfence_vvma_all();
        ret
    }
}

/// Fetches the guest instruction at the given guest virtual address.
#[inline(always)]
pub(crate) fn fetch_guest_instruction(gva: GuestVirtAddr) -> u32 {
    let mut inst = 0u32;
    let _ = unsafe {
        // we can never get -1 now, as exception handling is not implemented
        _fetch_guest_instruction(gva.as_usize(), &mut inst as *mut u32)
    };
    inst
}
