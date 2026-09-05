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

/// Breakpoint trap hook type.
pub type BreakpointHandler = fn(&mut KernelTrapFrame<'_>) -> bool;

/// Debug trap hook type.
pub type DebugHandler = fn(&mut KernelTrapFrame<'_>) -> bool;

fn default_irq_handler(irq: usize) -> bool {
    trace!("IRQ {} triggered", irq);
    false
}

fn default_page_fault_handler(addr: VirtAddr, flags: PageFaultFlags) -> bool {
    warn!("Page fault at {:#x} with flags {:?}", addr, flags);
    false
}

fn default_breakpoint_handler(_tf: &mut KernelTrapFrame<'_>) -> bool {
    false
}

fn default_debug_handler(_tf: &mut KernelTrapFrame<'_>) -> bool {
    false
}

static IRQ_HANDLER: AtomicUsize = AtomicUsize::new(0);
static PAGE_FAULT_HANDLER: AtomicUsize = AtomicUsize::new(0);
static BREAKPOINT_HANDLER: AtomicUsize = AtomicUsize::new(0);
static DEBUG_HANDLER: AtomicUsize = AtomicUsize::new(0);

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

/// Installs the global breakpoint trap hook and returns the previous one.
pub fn set_breakpoint_handler(handler: BreakpointHandler) -> BreakpointHandler {
    let old = BREAKPOINT_HANDLER.swap(handler as usize, Ordering::AcqRel);
    if old == 0 {
        default_breakpoint_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `BreakpointHandler`.
        unsafe { core::mem::transmute::<usize, BreakpointHandler>(old) }
    }
}

/// Installs the global debug trap hook and returns the previous one.
pub fn set_debug_handler(handler: DebugHandler) -> DebugHandler {
    let old = DEBUG_HANDLER.swap(handler as usize, Ordering::AcqRel);
    if old == 0 {
        default_debug_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `DebugHandler`.
        unsafe { core::mem::transmute::<usize, DebugHandler>(old) }
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

/// Dispatches an IRQ to the installed trap hook.
pub fn irq_handler(irq: usize) -> bool {
    dispatch_irq(irq)
}

/// Dispatches a page fault to the installed trap hook.
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
pub fn breakpoint_handler(tf: &mut KernelTrapFrame<'_>) -> bool {
    let handler = BREAKPOINT_HANDLER.load(Ordering::Acquire);
    let handler = if handler == 0 {
        default_breakpoint_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `BreakpointHandler`.
        unsafe { core::mem::transmute::<usize, BreakpointHandler>(handler) }
    };
    handler(tf)
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
pub fn debug_handler(tf: &mut KernelTrapFrame<'_>) -> bool {
    let handler = DEBUG_HANDLER.load(Ordering::Acquire);
    let handler = if handler == 0 {
        default_debug_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `DebugHandler`.
        unsafe { core::mem::transmute::<usize, DebugHandler>(handler) }
    };
    handler(tf)
}
