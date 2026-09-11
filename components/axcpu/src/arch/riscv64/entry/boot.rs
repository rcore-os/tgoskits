//! Supervisor boot trap entry without runtime CPU-local state.

use core::mem::{offset_of, size_of};

use crate::registers::GeneralRegisters;

#[repr(C, align(16))]
struct BootFrame {
    registers: GeneralRegisters,
    status: usize,
    pc: usize,
    cause: usize,
    value: usize,
}

core::arch::global_asm!(
    include_asm_macros!(), include_str!("boot.S"),
    frame_size = const size_of::<BootFrame>(),
    sp = const offset_of!(GeneralRegisters, sp),
    status = const offset_of!(BootFrame, status), pc = const offset_of!(BootFrame, pc),
    cause = const offset_of!(BootFrame, cause), value = const offset_of!(BootFrame, value),
    dispatch = sym dispatch,
);

unsafe extern "C" fn dispatch(frame: *const BootFrame) {
    // SAFETY: the supervisor vector initialized and retains this aligned image.
    // It passes copied register values to policy and never exposes the stack pointer.
    let frame = unsafe { &*frame };
    crate::trap::boot::boot_trap_handler::handle(&crate::trap::boot::BootException {
        registers: frame.registers,
        status: frame.status as u64,
        pc: frame.pc,
        sp: frame.registers.sp,
        syndrome: frame.cause as u64,
        fault_address: crate::VirtAddr::from_usize(frame.value),
    });
}

pub(crate) fn vector() -> usize {
    unsafe extern "C" {
        fn __ax_cpu_riscv_boot_vector();
    }
    let address;
    // SAFETY: materialize the running address without requiring relocated data.
    unsafe {
        core::arch::asm!("lla {}, {}", out(reg) address, sym __ax_cpu_riscv_boot_vector, options(nomem, nostack));
    }
    address
}
