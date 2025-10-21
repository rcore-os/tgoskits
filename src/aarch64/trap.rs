use aarch64_cpu::registers::{ESR_EL1, FAR_EL1};
use tock_registers::interfaces::Readable;

use super::TrapFrame;
use crate::trap::PageFaultFlags;

#[repr(u8)]
#[derive(Debug)]
pub(super) enum TrapKind {
    Synchronous = 0,
    Irq = 1,
    Fiq = 2,
    SError = 3,
}

#[repr(u8)]
#[derive(Debug)]
enum TrapSource {
    CurrentSpEl0 = 0,
    CurrentSpElx = 1,
    LowerAArch64 = 2,
    LowerAArch32 = 3,
}

core::arch::global_asm!(
    include_str!("trap.S"),
    trapframe_size = const core::mem::size_of::<TrapFrame>(),
    TRAP_KIND_SYNC = const TrapKind::Synchronous as u8,
    TRAP_KIND_IRQ = const TrapKind::Irq as u8,
    TRAP_KIND_FIQ = const TrapKind::Fiq as u8,
    TRAP_KIND_SERROR = const TrapKind::SError as u8,
    TRAP_SRC_CURR_EL0 = const TrapSource::CurrentSpEl0 as u8,
    TRAP_SRC_CURR_ELX = const TrapSource::CurrentSpElx as u8,
    TRAP_SRC_LOWER_AARCH64 = const TrapSource::LowerAArch64 as u8,
    TRAP_SRC_LOWER_AARCH32 = const TrapSource::LowerAArch32 as u8,
);

#[inline(always)]
pub(super) fn is_valid_page_fault(iss: u64) -> bool {
    // Only handle Translation fault and Permission fault
    matches!(iss & 0b111100, 0b0100 | 0b1100) // IFSC or DFSC bits
}

fn handle_page_fault(tf: &mut TrapFrame, access_flags: PageFaultFlags) {
    let vaddr = va!(FAR_EL1.get() as usize);
    if handle_trap!(PAGE_FAULT, vaddr, access_flags) {
        return;
    }
    #[cfg(feature = "uspace")]
    if tf.fixup_exception() {
        return;
    }
    core::hint::cold_path();
    panic!(
        "Unhandled EL1 Page Fault @ {:#x}, fault_vaddr={:#x}, ESR={:#x} ({:?}):\n{:#x?}\n{}",
        tf.elr,
        vaddr,
        ESR_EL1.get(),
        access_flags,
        tf,
        tf.backtrace()
    );
}

#[unsafe(no_mangle)]
fn aarch64_trap_handler(tf: &mut TrapFrame, kind: TrapKind, source: TrapSource) {
    if matches!(
        source,
        TrapSource::CurrentSpEl0 | TrapSource::LowerAArch64 | TrapSource::LowerAArch32
    ) {
        panic!(
            "Invalid exception {:?} from {:?}:\n{:#x?}",
            kind, source, tf
        );
    }
    match kind {
        TrapKind::Fiq | TrapKind::SError => {
            panic!("Unhandled exception {:?}:\n{:#x?}", kind, tf);
        }
        TrapKind::Irq => {
            handle_trap!(IRQ, 0);
        }
        TrapKind::Synchronous => {
            let esr = ESR_EL1.extract();
            let iss = esr.read(ESR_EL1::ISS);
            match esr.read_as_enum(ESR_EL1::EC) {
                Some(ESR_EL1::EC::Value::InstrAbortCurrentEL) if is_valid_page_fault(iss) => {
                    handle_page_fault(tf, PageFaultFlags::EXECUTE);
                }
                Some(ESR_EL1::EC::Value::DataAbortCurrentEL) if is_valid_page_fault(iss) => {
                    let wnr = (iss & (1 << 6)) != 0; // WnR: Write not Read
                    let cm = (iss & (1 << 8)) != 0; // CM: Cache maintenance
                    handle_page_fault(
                        tf,
                        if wnr & !cm {
                            PageFaultFlags::WRITE
                        } else {
                            PageFaultFlags::READ
                        },
                    );
                }
                Some(ESR_EL1::EC::Value::Brk64) => {
                    debug!("BRK #{:#x} @ {:#x} ", iss, tf.elr);
                    tf.elr += 4;
                }
                e => {
                    let vaddr = va!(FAR_EL1.get() as usize);
                    panic!(
                        "Unhandled synchronous exception {:?} @ {:#x}: ESR={:#x} (EC {:#08b}, FAR: {:#x} ISS {:#x})\n{}",
                        e,
                        tf.elr,
                        esr.get(),
                        esr.read(ESR_EL1::EC),
                        vaddr,
                        iss,
                        tf.backtrace()
                    );
                }
            }
        }
    }
}
