//! Trap handling.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_memory_addr::VirtAddr;

pub use crate::{KernelTrapFrame, UserRegisters};

bitflags::bitflags! {
    /// Access information reported by a CPU page-fault trap.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct PageFaultFlags: usize {
        /// The faulting access was a read.
        const READ = 1 << 0;
        /// The faulting access was a write.
        const WRITE = 1 << 1;
        /// The faulting access was an instruction fetch.
        const EXECUTE = 1 << 2;
        /// The fault came from a less-privileged user context.
        const USER = 1 << 3;
    }
}

/// Privilege domain that owns a saved register image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapOrigin {
    /// The trap interrupted kernel execution.
    Kernel,
    /// The trap interrupted a less-privileged user context.
    User,
}

/// IRQ trap hook type.
pub type IrqHandler = fn(usize) -> bool;

/// Page-fault trap hook type.
pub type PageFaultHandler = fn(VirtAddr, PageFaultFlags) -> bool;

fn default_irq_handler(irq: usize) -> bool {
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

/// Dispatches an IRQ through the runtime-registered handler, or the default handler.
pub fn dispatch_irq(irq: usize) -> bool {
    let handler = IRQ_HANDLER.load(Ordering::Acquire);
    let handler = if handler == 0 {
        default_irq_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `IrqHandler`.
        unsafe { core::mem::transmute::<usize, IrqHandler>(handler) }
    };
    handler(irq)
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
pub fn irq_handler(irq: usize) -> bool {
    dispatch_irq(irq)
}

/// Page fault handler.
#[eii]
pub fn page_fault_handler(addr: VirtAddr, flags: PageFaultFlags) -> bool {
    dispatch_page_fault(addr, flags)
}

/// Handles a kernel page fault while preserving the standard fixup sequence.
///
/// The saved instruction PC is updated when either the nofault or ordinary
/// exception table contains a matching recovery entry. The page-fault hook is
/// invoked between those two fixup attempts with the IRQ state inherited from
/// the interrupted context.
pub fn handle_kernel_page_fault(
    saved_pc: &mut usize,
    addr: VirtAddr,
    flags: PageFaultFlags,
    parent_irqs_enabled: bool,
) -> bool {
    handle_kernel_page_fault_with(
        saved_pc,
        addr,
        flags,
        parent_irqs_enabled,
        fixup_nofault_exception_ip,
        call_page_fault_handler_with_parent_irqs,
        fixup_exception_ip,
    )
}

fn handle_kernel_page_fault_with<N, D, O>(
    saved_pc: &mut usize,
    addr: VirtAddr,
    flags: PageFaultFlags,
    parent_irqs_enabled: bool,
    mut fixup_nofault: N,
    mut dispatch: D,
    mut fixup_ordinary: O,
) -> bool
where
    N: FnMut(&mut usize) -> bool,
    D: FnMut(VirtAddr, PageFaultFlags, bool) -> bool,
    O: FnMut(&mut usize) -> bool,
{
    fixup_nofault(saved_pc)
        || dispatch(addr, flags, parent_irqs_enabled)
        || fixup_ordinary(saved_pc)
}

#[inline]
fn fixup_nofault_exception_ip(saved_pc: &mut usize) -> bool {
    #[cfg(feature = "exception-table")]
    {
        crate::exception_table::fixup_nofault_exception_ip(saved_pc)
    }
    #[cfg(not(feature = "exception-table"))]
    {
        let _ = saved_pc;
        false
    }
}

#[inline]
fn fixup_exception_ip(saved_pc: &mut usize) -> bool {
    #[cfg(feature = "exception-table")]
    {
        crate::exception_table::fixup_exception_ip(saved_pc)
    }
    #[cfg(not(feature = "exception-table"))]
    {
        let _ = saved_pc;
        false
    }
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
#[eii]
pub fn debug_handler(_tf: &mut KernelTrapFrame<'_>) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn kernel_page_fault_runs_fixup_dispatch_fixup_in_order() {
        let mut saved_pc = 0x1000;
        let events = RefCell::new(std::vec::Vec::new());

        let handled = handle_kernel_page_fault_with(
            &mut saved_pc,
            va!(0x2000),
            PageFaultFlags::READ,
            true,
            |_| {
                events.borrow_mut().push("nofault");
                false
            },
            |_, _, parent_irqs_enabled| {
                assert!(parent_irqs_enabled);
                events.borrow_mut().push("dispatch");
                false
            },
            |_| {
                events.borrow_mut().push("ordinary");
                false
            },
        );

        assert!(!handled);
        assert_eq!(*events.borrow(), ["nofault", "dispatch", "ordinary"]);
        assert_eq!(saved_pc, 0x1000);
    }

    #[test]
    fn kernel_page_fault_nofault_fixup_short_circuits_and_updates_pc() {
        let mut saved_pc = 0x1000;
        let handled = handle_kernel_page_fault_with(
            &mut saved_pc,
            va!(0x2000),
            PageFaultFlags::WRITE,
            false,
            |pc| {
                *pc = 0x1100;
                true
            },
            |_, _, _| panic!("dispatch must not run after a nofault fixup"),
            |_| panic!("ordinary fixup must not run after a nofault fixup"),
        );

        assert!(handled);
        assert_eq!(saved_pc, 0x1100);
    }

    #[test]
    fn kernel_page_fault_handled_lazy_fault_short_circuits() {
        let mut saved_pc = 0x1000;
        let handled = handle_kernel_page_fault_with(
            &mut saved_pc,
            va!(0x2000),
            PageFaultFlags::EXECUTE,
            true,
            |_| false,
            |addr, flags, parent_irqs_enabled| {
                assert_eq!(addr, va!(0x2000));
                assert_eq!(flags, PageFaultFlags::EXECUTE);
                assert!(parent_irqs_enabled);
                true
            },
            |_| panic!("ordinary fixup must not run after dispatch handles the fault"),
        );

        assert!(handled);
        assert_eq!(saved_pc, 0x1000);
    }

    #[test]
    fn kernel_page_fault_ordinary_fixup_updates_pc() {
        let mut saved_pc = 0x1000;
        let handled = handle_kernel_page_fault_with(
            &mut saved_pc,
            va!(0x2000),
            PageFaultFlags::READ,
            false,
            |_| false,
            |_, _, parent_irqs_enabled| {
                assert!(!parent_irqs_enabled);
                false
            },
            |pc| {
                *pc = 0x1200;
                true
            },
        );

        assert!(handled);
        assert_eq!(saved_pc, 0x1200);
    }

    #[test]
    fn kernel_page_fault_returns_unhandled_when_all_stages_decline() {
        let mut saved_pc = 0x1000;
        let handled = handle_kernel_page_fault_with(
            &mut saved_pc,
            va!(0x2000),
            PageFaultFlags::READ,
            false,
            |_| false,
            |_, _, _| false,
            |_| false,
        );

        assert!(!handled);
        assert_eq!(saved_pc, 0x1000);
    }
}
