//! Trap handling.

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_memory_addr::VirtAddr;
pub use ax_page_table_entry::MappingFlags as PageFaultFlags;

pub use crate::{KernelTrapFrame, UserRegisters};

/// Privilege domain that owns a saved register image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapOrigin {
    /// The trap interrupted kernel execution.
    Kernel,
    /// The trap interrupted a less-privileged user context.
    User,
}

/// Linear proof that an IRQ dispatcher is completing an architecture trap.
///
/// The permit carries no register state. Its ownership proves that the caller
/// still has the architecture continuation which will restore the interrupted
/// raw IRQ state. It is deliberately neither cloneable nor transferable to
/// another CPU.
#[must_use = "a trap IRQ permit must be consumed by the registered IRQ handler"]
#[derive(Debug)]
pub struct TrapIrqPermit {
    vector: usize,
    not_send: PhantomData<*mut ()>,
}

impl TrapIrqPermit {
    /// Creates a permit at a raw architecture IRQ-entry boundary.
    ///
    /// # Safety
    ///
    /// The caller must be executing the unique continuation of a real IRQ
    /// exception with raw local IRQs masked. That continuation must own the
    /// saved interrupted IRQ state and restore it after this permit is consumed.
    /// A VM-exit or ordinary task-context callback does not satisfy this
    /// contract, even when it happens to observe IRQs masked.
    pub unsafe fn from_arch_entry(vector: usize) -> Self {
        Self {
            vector,
            not_send: PhantomData,
        }
    }

    /// Returns the architecture vector or cause bound to this trap entry.
    pub const fn vector(&self) -> usize {
        self.vector
    }
}

/// IRQ trap hook type.
pub type IrqHandler = fn(TrapIrqPermit) -> bool;

/// Page-fault trap hook type.
pub type PageFaultHandler = fn(VirtAddr, PageFaultFlags) -> bool;

fn default_irq_handler(permit: TrapIrqPermit) -> bool {
    let irq = permit.vector();
    trace!("IRQ {} triggered", irq);
    false
}

fn default_page_fault_handler(addr: VirtAddr, flags: PageFaultFlags) -> bool {
    warn!("Page fault at {:#x} with flags {:?}", addr, flags);
    false
}

static IRQ_HANDLER: AtomicUsize = AtomicUsize::new(0);
static PAGE_FAULT_HANDLER: AtomicUsize = AtomicUsize::new(0);

/// Installs the global IRQ trap hook and returns the previous one.
pub fn set_irq_handler(handler: IrqHandler) -> IrqHandler {
    let old = IRQ_HANDLER.swap(handler as usize, Ordering::AcqRel);
    if old == 0 {
        default_irq_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `IrqHandler`.
        unsafe { core::mem::transmute::<usize, IrqHandler>(old) }
    }
}

/// Installs the global page-fault trap hook and returns the previous one.
pub fn set_page_fault_handler(handler: PageFaultHandler) -> PageFaultHandler {
    let old = PAGE_FAULT_HANDLER.swap(handler as usize, Ordering::AcqRel);
    if old == 0 {
        default_page_fault_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `PageFaultHandler`.
        unsafe { core::mem::transmute::<usize, PageFaultHandler>(old) }
    }
}

/// Dispatches an IRQ through the runtime-registered trap handler.
///
/// The consumed [`TrapIrqPermit`] prevents task and VM-exit code from selecting
/// the architecture trap-return scheduler path merely by inspecting the live
/// IRQ mask.
pub fn dispatch_irq(permit: TrapIrqPermit) -> bool {
    let handler = IRQ_HANDLER.load(Ordering::Acquire);
    let handler = if handler == 0 {
        default_irq_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `IrqHandler`.
        unsafe { core::mem::transmute::<usize, IrqHandler>(handler) }
    };
    handler(permit)
}

/// Dispatches an IRQ directly from one of this crate's architecture entries.
///
/// # Safety
///
/// The caller must satisfy [`TrapIrqPermit::from_arch_entry`]'s contract.
pub(crate) unsafe fn dispatch_arch_irq(irq: usize) -> bool {
    // SAFETY: the caller proves this is the unique raw architecture entry.
    let permit = unsafe { TrapIrqPermit::from_arch_entry(irq) };
    dispatch_irq(permit)
}

/// Dispatches a page fault through the runtime-registered handler, or the default handler.
pub fn dispatch_page_fault(addr: VirtAddr, flags: PageFaultFlags) -> bool {
    let handler = PAGE_FAULT_HANDLER.load(Ordering::Acquire);
    let handler = if handler == 0 {
        default_page_fault_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `PageFaultHandler`.
        unsafe { core::mem::transmute::<usize, PageFaultHandler>(handler) }
    };
    handler(addr, flags)
}

/// IRQ handler.
#[eii]
pub fn irq_handler(permit: TrapIrqPermit) -> bool {
    dispatch_irq(permit)
}

#[cfg(test)]
mod irq_permit_tests {
    use super::*;

    macro_rules! assert_not_impl {
        ($tested_type:ty, $tested_trait:path) => {
            const _: fn() = || {
                trait AmbiguousIfImplemented<Marker> {
                    fn check() {}
                }

                impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}

                struct Implemented;
                impl<T: ?Sized + $tested_trait> AmbiguousIfImplemented<Implemented> for T {}

                let _ = <$tested_type as AmbiguousIfImplemented<_>>::check;
            };
        };
    }

    assert_not_impl!(TrapIrqPermit, Send);
    assert_not_impl!(TrapIrqPermit, Clone);
    assert_not_impl!(TrapIrqPermit, Copy);
}

/// Page fault handler.
#[eii]
pub fn page_fault_handler(addr: VirtAddr, flags: PageFaultFlags) -> bool {
    dispatch_page_fault(addr, flags)
}

/// Invoke the page-fault slow path with the IRQ state restored to the
/// faulting context.
#[inline]
pub(crate) fn call_page_fault_handler_with_parent_irqs(
    addr: VirtAddr,
    flags: PageFaultFlags,
    parent_irqs_enabled: bool,
) -> bool {
    if parent_irqs_enabled {
        crate::asm::enable_irqs();
    }
    let handled = page_fault_handler(addr, flags);
    if parent_irqs_enabled {
        crate::asm::disable_irqs();
    }
    handled
}

/// Breakpoint handler.
///
/// The handler is invoked with a typed view of the trapped kernel registers
/// and must return a boolean indicating whether it has fully handled the trap:
///
/// - `true` means the breakpoint has been handled and control should resume
///   according to the state encoded in the trap frame.
/// - `false` means the breakpoint was not handled and default processing
///   (such as falling back to another mechanism or terminating) should occur.
///
/// When returning `true`, the handler is responsible for updating the saved
/// program counter (or equivalent PC field) in the trap frame as required by
/// the target architecture. In particular, the handler must ensure that,
/// upon resuming from the trap, execution does not immediately re-trigger the
/// same breakpoint instruction or condition, which could otherwise lead to an
/// infinite trap loop. Register changes must go through
/// [`KernelTrapFrame::apply_registers`], which preserves CPU-owned and
/// privilege-origin state.
#[eii]
pub fn breakpoint_handler(_tf: &mut KernelTrapFrame<'_>) -> bool {
    false
}

/// Debug handler.
///
/// On `x86_64`, the handler is invoked for debug-related traps (for
/// example, hardware breakpoints, single-step traps, or other debug
/// exceptions). The handler receives a typed kernel-register view and returns
/// a boolean with the following meaning:
///
/// - `true` means the debug trap has been fully handled and execution should
///   resume from the state stored in the trap frame.
/// - `false` means the debug trap was not handled and default/secondary
///   processing should take place.
///
/// As with [`breakpoint_handler()`], when returning `true`, the handler must adjust
/// the saved program counter (or equivalent) in the trap frame if required by
/// the architecture so that resuming execution does not immediately cause the
/// same debug condition to fire again. Callers must take the architecture-
/// specific PC semantics into account when deciding how to advance or modify
/// the PC. Register changes must go through
/// [`KernelTrapFrame::apply_registers`], which preserves CPU-owned and
/// privilege-origin state.
#[cfg(target_arch = "x86_64")]
#[eii]
pub fn debug_handler(_tf: &mut KernelTrapFrame<'_>) -> bool {
    false
}
