use aarch64_cpu::registers::*;
use tock_registers::interfaces::Readable;

use super::TrapFrame;
use crate::trap::PageFaultFlags;

#[repr(u8)]
#[derive(Debug)]
pub(super) enum TrapKind {
    Synchronous = 0,
    Irq         = 1,
    Fiq         = 2,
    SError      = 3,
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
    #[cfg(not(feature = "arm-el2"))]
    include_str!("trap.S"),
    #[cfg(feature = "arm-el2")]
    concat!(".equ arm_el2, 1\n", include_str!("trap.S")),
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

#[inline(always)]
fn fault_addr() -> usize {
    #[cfg(not(feature = "arm-el2"))]
    {
        FAR_EL1.get() as usize
    }

    #[cfg(feature = "arm-el2")]
    {
        FAR_EL2.get() as usize
    }
}

#[inline(always)]
fn esr_value() -> u64 {
    #[cfg(not(feature = "arm-el2"))]
    {
        ESR_EL1.get()
    }

    #[cfg(feature = "arm-el2")]
    {
        ESR_EL2.get()
    }
}

fn handle_breakpoint(tf: &mut TrapFrame) {
    if crate::trap::breakpoint_handler(tf) {
        return;
    }
    tf.elr += 4;
}

fn handle_page_fault(tf: &mut TrapFrame, access_flags: PageFaultFlags) {
    let vaddr = va!(fault_addr());
    if crate::trap::page_fault_handler(vaddr, access_flags) {
        return;
    }
    #[cfg(feature = "uspace")]
    if tf.fixup_exception() {
        return;
    }
    panic!(
        "Unhandled Page Fault @ {:#x}, fault_vaddr={:#x}, ESR={:#x} ({:?}):\n{:#x?}\n{}",
        tf.elr,
        vaddr,
        esr_value(),
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
            crate::trap::irq_handler(0);
        }
        TrapKind::Synchronous => {
            #[cfg(not(feature = "arm-el2"))]
            let esr = ESR_EL1.extract();
            #[cfg(feature = "arm-el2")]
            let esr = ESR_EL2.extract();

            #[cfg(not(feature = "arm-el2"))]
            let iss = esr.read(ESR_EL1::ISS);
            #[cfg(feature = "arm-el2")]
            let iss = esr.read(ESR_EL2::ISS);

            #[cfg(not(feature = "arm-el2"))]
            let ec = esr.read_as_enum(ESR_EL1::EC);
            #[cfg(feature = "arm-el2")]
            let ec = esr.read_as_enum(ESR_EL2::EC);

            match ec {
                #[cfg(not(feature = "arm-el2"))]
                Some(ESR_EL1::EC::Value::InstrAbortCurrentEL) if is_valid_page_fault(iss) => {
                    handle_page_fault(tf, PageFaultFlags::EXECUTE);
                }
                #[cfg(feature = "arm-el2")]
                Some(ESR_EL2::EC::Value::InstrAbortCurrentEL) if is_valid_page_fault(iss) => {
                    handle_page_fault(tf, PageFaultFlags::EXECUTE);
                }
                #[cfg(not(feature = "arm-el2"))]
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
                #[cfg(feature = "arm-el2")]
                Some(ESR_EL2::EC::Value::DataAbortCurrentEL) if is_valid_page_fault(iss) => {
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
                #[cfg(not(feature = "arm-el2"))]
                Some(ESR_EL1::EC::Value::Brk64) => {
                    debug!("BRK #{:#x} @ {:#x} ", iss, tf.elr);
                    handle_breakpoint(tf);
                }
                #[cfg(feature = "arm-el2")]
                Some(ESR_EL2::EC::Value::Brk64) => {
                    debug!("BRK #{:#x} @ {:#x} ", iss, tf.elr);
                    handle_breakpoint(tf);
                }
                e => {
                    let vaddr = va!(fault_addr());

                    #[cfg(not(feature = "arm-el2"))]
                    let ec_bits = esr.read(ESR_EL1::EC);
                    #[cfg(feature = "arm-el2")]
                    let ec_bits = esr.read(ESR_EL2::EC);

                    panic!(
                        "Unhandled synchronous exception {:?} @ {:#x}: ESR={:#x} (EC {:#08b}, \
                         FAR: {:#x} ISS {:#x})\n{}",
                        e,
                        tf.elr,
                        esr.get(),
                        ec_bits,
                        vaddr,
                        iss,
                        tf.backtrace()
                    );
                }
            }
        }
    }
}
