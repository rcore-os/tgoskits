//! Early PLV0 exception vectors without runtime CPU-local requirements.

use core::mem::{offset_of, size_of};

use super::super::context::TrapFrame;

core::arch::global_asm!(
    include_asm_macros!(), include_str!("boot.S"),
    frame_size = const size_of::<TrapFrame>(),
    sp_offset = const offset_of!(TrapFrame, regs) + offset_of!(crate::registers::GeneralRegisters, sp),
    prmd_offset = const offset_of!(TrapFrame, prmd),
    era_offset = const offset_of!(TrapFrame, era),
    dispatch = sym dispatch,
);

unsafe extern "C" fn dispatch(frame: *const TrapFrame) {
    // SAFETY: the PLV0 boot vector initialized this aligned stack image and
    // retains it through this callback; copied values escape, never references.
    let frame = unsafe { &*frame };
    // SAFETY: synchronous vector entry at PLV0 owns the current exception bank.
    let (syndrome, badv) = unsafe {
        (
            crate::registers::read_csr::<5>(),
            crate::registers::read_csr::<7>(),
        )
    };
    crate::trap::boot::boot_trap_handler::handle(&crate::trap::boot::BootException {
        registers: frame.regs,
        pc: frame.era,
        sp: frame.regs.sp,
        status: frame.prmd as u64,
        syndrome: syndrome as u64,
        fault_address: crate::VirtAddr::from_usize(badv),
    });
}

pub(crate) fn vector() -> usize {
    unsafe extern "C" {
        fn __ax_cpu_boot_vector();
    }
    let address;
    // SAFETY: PC-relative materialization remains valid before relocation.
    unsafe {
        core::arch::asm!("la.pcrel {}, {}", out(reg) address, sym __ax_cpu_boot_vector, options(nomem, nostack))
    };
    address
}
