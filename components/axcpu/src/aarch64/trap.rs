use aarch64_cpu::registers::*;
use tock_registers::interfaces::Readable;

use super::TrapFrame;
use crate::{TrapOrigin, trap::PageFaultFlags};

/// Untrusted register image produced and consumed by trap assembly.
#[repr(transparent)]
struct RawTrapFrame(TrapFrame);

const _: () = {
    assert!(core::mem::size_of::<RawTrapFrame>() == core::mem::size_of::<TrapFrame>());
    assert!(core::mem::align_of::<RawTrapFrame>() == core::mem::align_of::<TrapFrame>());
};

/// Lifetime-bound view of a kernel-origin AArch64 trap frame.
pub struct KernelTrapFrame<'a> {
    raw: &'a mut RawTrapFrame,
    _not_send: core::marker::PhantomData<*mut ()>,
}

impl<'a> KernelTrapFrame<'a> {
    /// Returns the privilege domain represented by this view.
    pub const fn origin(&self) -> TrapOrigin {
        TrapOrigin::Kernel
    }

    /// Copies the saved register image for inspection or probe emulation.
    pub const fn snapshot(&self) -> TrapFrame {
        self.raw.0
    }

    /// Applies task-register changes while preserving origin and saved SP.
    pub fn apply_registers(&mut self, updated: &TrapFrame) {
        const MODE_MASK: u64 = 0b1_1111;
        let saved_mode = self.raw.0.spsr & MODE_MASK;
        let sp = self.raw.0.sp;
        self.raw.0 = *updated;
        self.raw.0.spsr = (self.raw.0.spsr & !MODE_MASK) | saved_mode;
        self.raw.0.sp = sp;
    }

    /// Returns the saved instruction pointer.
    pub const fn ip(&self) -> usize {
        self.raw.0.ip()
    }

    /// Sets the saved instruction pointer.
    pub const fn set_ip(&mut self, ip: usize) {
        self.raw.0.set_ip(ip);
    }

    /// Creates the typed view at the assembly boundary.
    ///
    /// # Safety
    ///
    /// `raw` must be the uniquely borrowed, live kernel-origin frame built by
    /// the AArch64 vector entry and must remain valid for `'a`.
    unsafe fn from_raw(raw: &'a mut RawTrapFrame) -> Self {
        debug_assert_eq!(raw.0.origin(), TrapOrigin::Kernel);
        Self {
            raw,
            _not_send: core::marker::PhantomData,
        }
    }
}

impl core::fmt::Debug for KernelTrapFrame<'_> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.snapshot().fmt(formatter)
    }
}

#[repr(u8)]
#[derive(Debug)]
pub(super) enum TrapKind {
    Synchronous = 0,
    Irq         = 1,
    Fiq         = 2,
    SError      = 3,
}

impl TrapKind {
    const fn from_raw(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Synchronous),
            1 => Some(Self::Irq),
            2 => Some(Self::Fiq),
            3 => Some(Self::SError),
            _ => None,
        }
    }
}

#[repr(u8)]
#[derive(Debug)]
enum TrapSource {
    CurrentSpEl0 = 0,
    CurrentSpElx = 1,
    LowerAArch64 = 2,
    LowerAArch32 = 3,
}

impl TrapSource {
    const fn from_raw(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::CurrentSpEl0),
            1 => Some(Self::CurrentSpElx),
            2 => Some(Self::LowerAArch64),
            3 => Some(Self::LowerAArch32),
            _ => None,
        }
    }
}

core::arch::global_asm!(
    #[cfg(not(feature = "arm-el2"))]
    include_str!("trap.S"),
    #[cfg(feature = "arm-el2")]
    concat!(".equ arm_el2, 1\n", include_str!("trap.S")),
    trapframe_size = const core::mem::size_of::<RawTrapFrame>(),
    current_thread_offset = const ax_cpu_local::CPU_AREA_CURRENT_THREAD_OFFSET,
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

fn handle_breakpoint(tf: &mut KernelTrapFrame<'_>) {
    if crate::trap::breakpoint_handler(tf) {
        return;
    }
    tf.set_ip(tf.ip() + 4);
}

fn handle_page_fault(tf: &mut KernelTrapFrame<'_>, access_flags: PageFaultFlags) {
    let vaddr = va!(fault_addr());
    if crate::trap::call_page_fault_handler_with_parent_irqs(
        vaddr,
        access_flags,
        tf.raw.0.spsr & (1 << 7) == 0,
    ) {
        return;
    }
    #[cfg(feature = "exception-table")]
    if tf.raw.0.fixup_exception() {
        return;
    }
    let snapshot = tf.snapshot();
    let bt = snapshot.backtrace();
    panic!(
        "Unhandled Page Fault @ {:#x}, fault_vaddr={:#x}, ESR={:#x} ({:?}):\n{:#x?}\n{}",
        tf.raw.0.elr,
        vaddr,
        esr_value(),
        access_flags,
        snapshot,
        bt.kind("trap")
    );
}

#[unsafe(no_mangle)]
unsafe extern "C" fn aarch64_trap_handler(raw: *mut RawTrapFrame, raw_kind: u8, raw_source: u8) {
    let kind = TrapKind::from_raw(raw_kind)
        .unwrap_or_else(|| panic!("invalid AArch64 trap kind {raw_kind:#x}"));
    let source = TrapSource::from_raw(raw_source)
        .unwrap_or_else(|| panic!("invalid AArch64 trap source {raw_source:#x}"));
    // SAFETY: the vector assembly passes its aligned, live stack frame and
    // retains exclusive ownership until this handler returns.
    let raw = unsafe { &mut *raw };
    if matches!(
        source,
        TrapSource::CurrentSpEl0 | TrapSource::LowerAArch64 | TrapSource::LowerAArch32
    ) {
        let bt = raw.0.backtrace();
        panic!(
            "Invalid exception {:?} from {:?}:\n{:#x?}\n{}",
            kind,
            source,
            raw.0,
            bt.kind("trap")
        );
    }
    let mut tf = unsafe { KernelTrapFrame::from_raw(raw) };
    match kind {
        TrapKind::Fiq | TrapKind::SError => {
            let snapshot = tf.snapshot();
            let bt = snapshot.backtrace();
            panic!(
                "Unhandled exception {:?}:\n{:#x?}\n{}",
                kind,
                snapshot,
                bt.kind("trap")
            );
        }
        TrapKind::Irq => {
            crate::trap::dispatch_irq(0);
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
                    handle_page_fault(&mut tf, PageFaultFlags::EXECUTE);
                }
                #[cfg(feature = "arm-el2")]
                Some(ESR_EL2::EC::Value::InstrAbortCurrentEL) if is_valid_page_fault(iss) => {
                    handle_page_fault(&mut tf, PageFaultFlags::EXECUTE);
                }
                #[cfg(not(feature = "arm-el2"))]
                Some(ESR_EL1::EC::Value::DataAbortCurrentEL) if is_valid_page_fault(iss) => {
                    let wnr = (iss & (1 << 6)) != 0; // WnR: Write not Read
                    let cm = (iss & (1 << 8)) != 0; // CM: Cache maintenance
                    handle_page_fault(
                        &mut tf,
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
                        &mut tf,
                        if wnr & !cm {
                            PageFaultFlags::WRITE
                        } else {
                            PageFaultFlags::READ
                        },
                    );
                }
                #[cfg(not(feature = "arm-el2"))]
                Some(ESR_EL1::EC::Value::Brk64) => {
                    debug!("BRK #{:#x} @ {:#x} ", iss, tf.raw.0.elr);
                    handle_breakpoint(&mut tf);
                }
                #[cfg(feature = "arm-el2")]
                Some(ESR_EL2::EC::Value::Brk64) => {
                    debug!("BRK #{:#x} @ {:#x} ", iss, tf.raw.0.elr);
                    handle_breakpoint(&mut tf);
                }
                e => {
                    let vaddr = va!(fault_addr());

                    #[cfg(not(feature = "arm-el2"))]
                    let ec_bits = esr.read(ESR_EL1::EC);
                    #[cfg(feature = "arm-el2")]
                    let ec_bits = esr.read(ESR_EL2::EC);

                    let snapshot = tf.snapshot();
                    let bt = snapshot.backtrace();
                    panic!(
                        "Unhandled synchronous exception {:?} @ {:#x}: ESR={:#x} (EC {:#08b}, \
                         FAR: {:#x} ISS {:#x})\n{}",
                        e,
                        tf.raw.0.elr,
                        esr.get(),
                        ec_bits,
                        vaddr,
                        iss,
                        bt.kind("trap")
                    );
                }
            }
        }
    }
}
