use aarch64_cpu::registers::*;
use tock_registers::interfaces::Readable;

use super::context::TrapFrame;
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// AArch64 vector entry kind.
pub enum TrapKind {
    /// A synchronous exception.
    Synchronous = 0,
    /// A physical or virtual IRQ.
    Irq         = 1,
    /// A fast interrupt.
    Fiq         = 2,
    /// An asynchronous system error.
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Execution domain selecting an AArch64 vector slot.
pub enum TrapSource {
    /// Current exception level using SP_EL0.
    CurrentSpEl0 = 0,
    /// Current exception level using its own stack.
    CurrentSpElx = 1,
    /// A lower level executing AArch64.
    LowerAArch64 = 2,
    /// A lower level executing AArch32.
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
    include_str!("entry/gpr.S"),
    include_str!("entry/trap.S"),
    trapframe_size = const core::mem::size_of::<RawTrapFrame>(),
    elr_offset = const core::mem::offset_of!(TrapFrame, elr),
    sp_offset = const core::mem::offset_of!(TrapFrame, sp),
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

fn handle_breakpoint(tf: &mut KernelTrapFrame<'_>) {
    if crate::trap::breakpoint_handler(tf) {
        return;
    }
    tf.set_ip(tf.ip() + 4);
}

fn handle_page_fault(
    tf: &mut KernelTrapFrame<'_>,
    access_flags: PageFaultFlags,
    esr: u64,
    far: usize,
) {
    let vaddr = va!(far);
    #[cfg(feature = "exception-table")]
    if tf.raw.0.fixup_nofault_exception() {
        return;
    }
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
    let bt = crate::trap::diagnostics::BacktraceDisplay(snapshot.backtrace_registers());
    panic!(
        "Unhandled Page Fault @ {:#x}, fault_vaddr={:#x}, ESR={:#x} ({:?}):\n{:#x?}\n{}",
        tf.raw.0.elr, vaddr, esr, access_flags, snapshot, bt
    );
}

#[unsafe(no_mangle)]
unsafe extern "C" fn aarch64_trap_handler(
    raw: *mut RawTrapFrame,
    raw_kind: u8,
    raw_source: u8,
    level: u8,
) {
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
        let bt = crate::trap::diagnostics::BacktraceDisplay(raw.0.backtrace_registers());
        panic!(
            "Invalid exception {:?} from {:?}:\n{:#x?}\n{}",
            kind, source, raw.0, bt
        );
    }
    let mut tf = unsafe { KernelTrapFrame::from_raw(raw) };
    match kind {
        TrapKind::Fiq | TrapKind::SError => {
            let snapshot = tf.snapshot();
            let bt = crate::trap::diagnostics::BacktraceDisplay(snapshot.backtrace_registers());
            panic!("Unhandled exception {:?}:\n{:#x?}\n{}", kind, snapshot, bt);
        }
        TrapKind::Irq => {
            crate::trap::dispatch_irq(
                0,
                crate::trap::TrapOrigin::Kernel,
                Some(raw.0.interrupted_context()),
            );
        }
        TrapKind::Synchronous => {
            // Capture the selected exception bank before invoking any host hook.
            let (esr, far) = match level {
                1 => (ESR_EL1.get(), FAR_EL1.get() as usize),
                2 => (ESR_EL2.get(), FAR_EL2.get() as usize),
                _ => panic!("invalid exception level {level}"),
            };
            let iss = esr & 0x01ff_ffff;
            let ec = (esr >> 26) & 0x3f;
            match ec {
                0x21 if is_valid_page_fault(iss) => {
                    handle_page_fault(&mut tf, PageFaultFlags::EXECUTE, esr, far);
                }
                0x25 if is_valid_page_fault(iss) => {
                    let write = iss & (1 << 6) != 0;
                    let cache_maintenance = iss & (1 << 8) != 0;
                    let access = if write && !cache_maintenance {
                        PageFaultFlags::WRITE
                    } else {
                        PageFaultFlags::READ
                    };
                    handle_page_fault(&mut tf, access, esr, far);
                }
                0x3c => handle_breakpoint(&mut tf),
                _ => {
                    let snapshot = tf.snapshot();
                    let bt =
                        crate::trap::diagnostics::BacktraceDisplay(snapshot.backtrace_registers());
                    panic!(
                        "Unhandled EL{level} synchronous exception @ {:#x}: ESR={esr:#x}, \
                         FAR={far:#x}\n{bt}",
                        tf.ip()
                    );
                }
            }
        }
    }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn __ax_cpu_boot_trap(raw: *const RawTrapFrame, kind: u8, source: u8, level: u8) {
    // SAFETY: the shared vector save sequence owns this aligned initialized
    // frame until the synchronous boot callback returns to its restore sequence.
    let frame = unsafe { &(*raw).0 };
    let (syndrome, fault_address) = match level {
        1 => (ESR_EL1.get(), FAR_EL1.get()),
        2 => (ESR_EL2.get(), FAR_EL2.get()),
        _ => panic!("invalid boot exception level"),
    };
    let exception = crate::trap::boot::BootException {
        registers: frame.x,
        pc: frame.elr as usize,
        sp: frame.sp as usize,
        status: frame.spsr,
        syndrome,
        fault_address: crate::VirtAddr::from_usize(fault_address as usize),
        level,
        kind: TrapKind::from_raw(kind).expect("CPU vector kind"),
        source: TrapSource::from_raw(source).expect("CPU vector source"),
    };
    crate::trap::boot::boot_trap_handler::handle(&exception);
}
