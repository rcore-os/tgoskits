use x86::{controlregs::cr2, irq::*};
use x86_64::{registers::rflags::RFlags, structures::idt::PageFaultErrorCode};

use super::{TrapFrame, gdt};
use crate::trap::PageFaultFlags;

core::arch::global_asm!(
    include_str!("trap.S"),
    trapframe_size = const core::mem::size_of::<TrapFrame>(),
    UDATA = const gdt::UDATA.0,
    UCODE64 = const gdt::UCODE64.0,
    SYSCALL_VECTOR = const LEGACY_SYSCALL_VECTOR,
);

pub(super) const LEGACY_SYSCALL_VECTOR: u8 = 0x80;
pub(super) const IRQ_VECTOR_START: u8 = 0x20;
pub(super) const IRQ_VECTOR_END: u8 = 0xff;

fn handle_page_fault(tf: &mut TrapFrame) {
    let access_flags = err_code_to_flags(tf.error_code)
        .unwrap_or_else(|e| panic!("Invalid #PF error code: {:#x}", e));
    let vaddr = va!(unsafe { cr2() });
    if crate::trap::call_page_fault_handler_with_parent_irqs(
        vaddr,
        access_flags,
        RFlags::from_bits_truncate(tf.rflags).contains(RFlags::INTERRUPT_FLAG),
    ) {
        return;
    }
    #[cfg(feature = "uspace")]
    if tf.fixup_exception() {
        return;
    }
    panic!(
        "Unhandled #PF @ {:#x}, fault_vaddr={:#x}, error_code={:#x} ({:?}):\n{:#x?}\n{}",
        tf.rip,
        vaddr,
        tf.error_code,
        access_flags,
        tf,
        tf.backtrace()
    );
}

fn handle_breakpoint(tf: &mut TrapFrame) {
    debug!("#BP @ {:#x} ", tf.rip);
    let _ = crate::trap::breakpoint_handler(tf);
}

fn handle_debug(tf: &mut TrapFrame) {
    debug!("#DB @ {:#x} ", tf.rip);
    if crate::trap::debug_handler(tf) {
        return;
    }
    panic!(
        "Unhandled #DB @ {:#x}, error_code={:#x}:\n{:#x?}\n{}",
        tf.rip,
        tf.error_code,
        tf,
        tf.backtrace()
    );
}

#[unsafe(no_mangle)]
fn x86_trap_handler(tf: &mut TrapFrame) {
    match tf.vector as u8 {
        PAGE_FAULT_VECTOR => handle_page_fault(tf),
        BREAKPOINT_VECTOR => handle_breakpoint(tf),
        DEBUG_VECTOR => handle_debug(tf),
        GENERAL_PROTECTION_FAULT_VECTOR => {
            panic!(
                "#GP @ {:#x}, error_code={:#x}:\n{:#x?}\n{}",
                tf.rip,
                tf.error_code,
                tf,
                tf.backtrace()
            );
        }
        IRQ_VECTOR_START..=IRQ_VECTOR_END => {
            crate::trap::irq_handler(tf.vector as _);
        }
        _ => {
            panic!(
                "Unhandled exception {} ({}, error_code={:#x}) @ {:#x}:\n{:#x?}\n{}",
                tf.vector,
                vec_to_str(tf.vector),
                tf.error_code,
                tf.rip,
                tf,
                tf.backtrace()
            );
        }
    }
}

fn vec_to_str(vec: u64) -> &'static str {
    if vec < 32 {
        EXCEPTIONS[vec as usize].mnemonic
    } else {
        "Unknown"
    }
}

pub(super) fn err_code_to_flags(err_code: u64) -> Result<PageFaultFlags, u64> {
    let code = PageFaultErrorCode::from_bits_truncate(err_code);
    let reserved_bits = (PageFaultErrorCode::CAUSED_BY_WRITE
        | PageFaultErrorCode::USER_MODE
        | PageFaultErrorCode::INSTRUCTION_FETCH
        | PageFaultErrorCode::PROTECTION_VIOLATION)
        .complement();
    if code.intersects(reserved_bits) {
        Err(err_code)
    } else {
        let mut flags = PageFaultFlags::empty();
        if code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
            flags |= PageFaultFlags::WRITE;
        } else {
            flags |= PageFaultFlags::READ;
        }
        if code.contains(PageFaultErrorCode::USER_MODE) {
            flags |= PageFaultFlags::USER;
        }
        if code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
            flags |= PageFaultFlags::EXECUTE;
        }
        Ok(flags)
    }
}
