#[cfg(feature = "fp-simd")]
use riscv::register::sstatus;
use riscv::{
    interrupt::{
        Trap,
        supervisor::{Exception as E, Interrupt as I},
    },
    register::{scause, stval},
};

use super::TrapFrame;
use crate::trap::PageFaultFlags;

core::arch::global_asm!(
    include_asm_macros!(),
    include_str!("trap.S"),
    trapframe_size = const core::mem::size_of::<TrapFrame>(),
);

fn handle_breakpoint(tf: &mut TrapFrame) {
    debug!("Exception(Breakpoint) @ {:#x} ", tf.sepc);
    if crate::trap::breakpoint_handler(tf) {
        return;
    }
    tf.sepc += 2;
}

fn handle_page_fault(tf: &mut TrapFrame, access_flags: PageFaultFlags) {
    let vaddr = va!(stval::read());
    if crate::trap::call_page_fault_handler_with_parent_irqs(vaddr, access_flags, tf.sstatus.spie())
    {
        return;
    }
    #[cfg(feature = "exception-table")]
    if tf.fixup_exception() {
        return;
    }
    let bt = tf.backtrace();
    panic!(
        "Unhandled Supervisor Page Fault @ {:#x}, fault_vaddr={:#x} ({:?}):\n{:#x?}\n{}",
        tf.sepc,
        vaddr,
        access_flags,
        tf,
        bt.kind("trap")
    );
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
            Trap::Exception(E::Breakpoint) => handle_breakpoint(tf),
            Trap::Interrupt(_) => {
                crate::trap::dispatch_irq(scause.bits());
            }
            _ => {
                let bt = tf.backtrace();
                panic!(
                    "Unhandled trap {:?} @ {:#x}, stval={:#x}:\n{:#x?}\n{}",
                    cause,
                    tf.sepc,
                    stval::read(),
                    tf,
                    bt.kind("trap")
                );
            }
        }
    } else {
        let bt = tf.backtrace();
        panic!(
            "Unknown trap {:#x?} @ {:#x}:\n{:#x?}\n{}",
            scause.cause(),
            tf.sepc,
            tf,
            bt.kind("trap")
        );
    }

    // Update tf.sstatus to preserve current hardware FS state
    // This replaces the assembly-level FS handling workaround
    #[cfg(feature = "fp-simd")]
    tf.sstatus.set_fs(sstatus::read().fs());
}
