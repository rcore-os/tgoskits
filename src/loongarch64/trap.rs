use loongArch64::register::{
    badv,
    estat::{self, Exception, Trap},
};

use super::context::TrapFrame;
use crate::trap::PageFaultFlags;

core::arch::global_asm!(
    include_asm_macros!(),
    include_str!("trap.S"),
    trapframe_size = const (core::mem::size_of::<TrapFrame>()),
);

fn handle_breakpoint(era: &mut usize) {
    debug!("Exception(Breakpoint) @ {era:#x} ");
    *era += 4;
}

fn handle_page_fault(tf: &mut TrapFrame, access_flags: PageFaultFlags) {
    let vaddr = va!(badv::read().vaddr());
    if core::hint::likely(handle_trap!(PAGE_FAULT, vaddr, access_flags)) {
        return;
    }
    if !tf.fixup_exception() {
        panic!(
            "Unhandled PLV0 Page Fault @ {:#x}, fault_vaddr={:#x} ({:?}):\n{:#x?}\n{}",
            tf.era,
            vaddr,
            access_flags,
            tf,
            tf.backtrace()
        );
    }
}

#[unsafe(no_mangle)]
fn loongarch64_trap_handler(tf: &mut TrapFrame) {
    let estat = estat::read();

    match estat.cause() {
        Trap::Exception(Exception::LoadPageFault)
        | Trap::Exception(Exception::PageNonReadableFault) => {
            handle_page_fault(tf, PageFaultFlags::READ)
        }
        Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::PageModifyFault) => {
            handle_page_fault(tf, PageFaultFlags::WRITE)
        }
        Trap::Exception(Exception::FetchPageFault)
        | Trap::Exception(Exception::PageNonExecutableFault) => {
            handle_page_fault(tf, PageFaultFlags::EXECUTE);
        }
        Trap::Exception(Exception::Breakpoint) => handle_breakpoint(&mut tf.era),
        Trap::Exception(Exception::AddressNotAligned) => unsafe {
            tf.emulate_unaligned().unwrap();
        },
        Trap::Interrupt(_) => {
            let irq_num: usize = estat.is().trailing_zeros() as usize;
            handle_trap!(IRQ, irq_num);
        }
        trap => {
            panic!(
                "Unhandled trap {:?} @ {:#x}:\n{:#x?}\n{}",
                trap,
                tf.era,
                tf,
                tf.backtrace()
            );
        }
    }
}
