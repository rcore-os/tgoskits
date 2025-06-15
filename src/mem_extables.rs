use core::arch::riscv64::hfence_vvma_all;
use riscv::register::vsatp::Vsatp;

core::arch::global_asm!(include_str!("mem_extable.S"));

unsafe extern "C" {
    /// Copy data from guest physical address to host memory.
    fn _copy_from_guest(dst: *mut u8, guest_paddr: *const u8, len: usize) -> usize;
    /// Copy data from host memory to guest physical address.
    fn _copy_to_guest(dst: *mut u8, src: *const u8, len: usize) -> usize;
}

/// This function copies data from guest physical memory to host memory.
pub(crate) fn copy_from_guest(dst: &mut [u8], gpa: usize) -> usize {
    let old_vsatp = riscv::register::vsatp::read().bits();
    unsafe {
        Vsatp::from_bits(0).write();
        hfence_vvma_all();
        let ret = _copy_from_guest(dst.as_mut_ptr(), gpa as *const u8, dst.len());
        Vsatp::from_bits(old_vsatp).write();
        hfence_vvma_all();
        ret
    }
}
/// This function copies data from host memory to guest physical memory.
pub(crate) fn copy_to_guest(src: &[u8], gpa: usize) -> usize {
    let old_vsatp = riscv::register::vsatp::read().bits();
    unsafe {
        Vsatp::from_bits(0).write();
        hfence_vvma_all();
        let ret = _copy_to_guest(gpa as *mut u8, src.as_ptr(), src.len());
        Vsatp::from_bits(old_vsatp).write();
        hfence_vvma_all();
        ret
    }
}
