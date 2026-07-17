use loongArch64::register::{
    badv,
    estat::{self, Exception, Trap},
};

use super::context::TrapFrame;
use crate::{TrapOrigin, trap::PageFaultFlags};

/// Untrusted register image produced and consumed by trap assembly.
#[repr(transparent)]
struct RawTrapFrame(TrapFrame);

const _: () = {
    assert!(core::mem::size_of::<RawTrapFrame>() == core::mem::size_of::<TrapFrame>());
    assert!(core::mem::align_of::<RawTrapFrame>() == core::mem::align_of::<TrapFrame>());
};

/// Lifetime-bound view of a PLV0-origin LoongArch trap frame.
///
/// The saved `u0` slot is only a diagnostic snapshot of the live kernel `r21`
/// CPU anchor. It is readable for diagnostics but is always preserved when
/// register changes are applied.
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

    /// Applies task-register changes while preserving the CPU anchor and
    /// privilege-return state.
    pub fn apply_registers(&mut self, updated: &TrapFrame) {
        const PPLV_MASK: usize = 0b11;
        let kernel_u0 = self.raw.0.regs.u0;
        let kernel_tp = self.raw.0.regs.tp;
        let saved_pplv = self.raw.0.prmd & PPLV_MASK;
        self.raw.0 = *updated;
        self.raw.0.regs.u0 = kernel_u0;
        self.raw.0.regs.tp = kernel_tp;
        self.raw.0.prmd = (self.raw.0.prmd & !PPLV_MASK) | saved_pplv;
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
    /// `raw` must be the uniquely borrowed, live PLV0-origin frame built by
    /// `exception_entry_base` and must remain valid for `'a`.
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

core::arch::global_asm!(
    include_asm_macros!(),
    include_str!("trap.S"),
    trapframe_size = const (core::mem::size_of::<RawTrapFrame>()),
);

fn handle_breakpoint(tf: &mut KernelTrapFrame<'_>) {
    debug!("Exception(Breakpoint) @ {:#x} ", tf.raw.0.era);
    if crate::trap::breakpoint_handler(tf) {
        return;
    }
    tf.set_ip(tf.ip() + 4);
}

fn handle_page_fault(tf: &mut KernelTrapFrame<'_>, access_flags: PageFaultFlags) {
    let vaddr = va!(badv::read().vaddr());
    if crate::trap::call_page_fault_handler_with_parent_irqs(
        vaddr,
        access_flags,
        tf.raw.0.prmd & (1 << 2) != 0,
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
        "Unhandled PLV0 Page Fault @ {:#x}, fault_vaddr={:#x} ({:?}):\n{:#x?}\n{}",
        tf.raw.0.era,
        vaddr,
        access_flags,
        snapshot,
        bt.kind("trap")
    );
}

#[unsafe(no_mangle)]
unsafe extern "C" fn loongarch64_trap_handler(raw: *mut RawTrapFrame) {
    // SAFETY: trap.S passes a complete aligned frame on the current kernel
    // stack and keeps it exclusively owned until this function returns.
    let raw = unsafe { &mut *raw };
    let mut tf = unsafe { KernelTrapFrame::from_raw(raw) };
    let estat = estat::read();

    match estat.cause() {
        Trap::Exception(Exception::LoadPageFault)
        | Trap::Exception(Exception::PageNonReadableFault) => {
            handle_page_fault(&mut tf, PageFaultFlags::READ)
        }
        Trap::Exception(Exception::StorePageFault)
        | Trap::Exception(Exception::PageModifyFault) => {
            handle_page_fault(&mut tf, PageFaultFlags::WRITE)
        }
        Trap::Exception(Exception::FetchPageFault)
        | Trap::Exception(Exception::PageNonExecutableFault) => {
            handle_page_fault(&mut tf, PageFaultFlags::EXECUTE);
        }
        Trap::Exception(Exception::Breakpoint) => handle_breakpoint(&mut tf),
        Trap::Exception(Exception::AddressNotAligned) => unsafe {
            let kernel_u0 = tf.raw.0.regs.u0;
            let result = tf.raw.0.emulate_unaligned();
            tf.raw.0.regs.u0 = kernel_u0;
            result.unwrap();
        },
        Trap::Interrupt(_) => {
            let irq_num: usize = estat.is().trailing_zeros() as usize;
            // SAFETY: the LoongArch exception entry owns the interrupted CRMD
            // state and returns through its architecture exception frame.
            unsafe { crate::trap::dispatch_arch_irq(irq_num) };
        }
        trap => {
            let snapshot = tf.snapshot();
            let bt = snapshot.backtrace();
            panic!(
                "Unhandled trap {:?} @ {:#x}:\n{:#x?}\n{}",
                trap,
                tf.raw.0.era,
                snapshot,
                bt.kind("trap")
            );
        }
    }
}
