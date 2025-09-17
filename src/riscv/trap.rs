use riscv::interrupt::supervisor::{Exception as E, Interrupt as I};
use riscv::interrupt::Trap;
#[cfg(feature = "fp-simd")]
use riscv::register::sstatus;
use riscv::register::{scause, stval};

use super::TrapFrame;
use crate::trap::PageFaultFlags;

core::arch::global_asm!(
    include_asm_macros!(),
    include_str!("trap.S"),
    trapframe_size = const core::mem::size_of::<TrapFrame>(),
);

fn handle_breakpoint(sepc: &mut usize) {
    debug!("Exception(Breakpoint) @ {sepc:#x} ");
    *sepc += 2
}

fn handle_page_fault(tf: &mut TrapFrame, access_flags: PageFaultFlags) {
    let vaddr = va!(stval::read());
    if core::hint::likely(handle_trap!(PAGE_FAULT, vaddr, access_flags)) {
        return;
    }
    if !tf.fixup_exception() {
        panic!(
            "Unhandled Supervisor Page Fault @ {:#x}, fault_vaddr={:#x} ({:?}):\n{:#x?}\n{}",
            tf.sepc,
            vaddr,
            access_flags,
            tf,
            tf.backtrace()
        );
    }
}

#[unsafe(no_mangle)]
fn riscv_trap_handler(tf: &mut TrapFrame) {
    let scause = scause::read();
    if let Ok(cause) = scause.cause().try_into::<I, E>() {
        match cause {
            Trap::Exception(E::LoadPageFault) => handle_page_fault(tf, PageFaultFlags::READ),
            Trap::Exception(E::StorePageFault) => handle_page_fault(tf, PageFaultFlags::WRITE),
            Trap::Exception(E::InstructionPageFault) => {
                handle_page_fault(tf, PageFaultFlags::EXECUTE)
            }
            Trap::Exception(E::Breakpoint) => handle_breakpoint(&mut tf.sepc),
            Trap::Interrupt(_) => {
                handle_trap!(IRQ, scause.bits());
            }
            _ => {
                panic!(
                    "Unhandled trap {:?} @ {:#x}, stval={:#x}:\n{:#x?}\n{}",
                    cause,
                    tf.sepc,
                    stval::read(),
                    tf,
                    tf.backtrace()
                );
            }
        }
    } else {
        panic!(
            "Unknown trap {:#x?} @ {:#x}:\n{:#x?}\n{}",
            scause.cause(),
            tf.sepc,
            tf,
            tf.backtrace()
        );
    }

    // Update tf.sstatus to preserve current hardware FS state
    // This replaces the assembly-level FS handling workaround
    #[cfg(feature = "fp-simd")]
    tf.sstatus.set_fs(sstatus::read().fs());
}
