//! Trap handling.

use ax_memory_addr::VirtAddr;
pub use ax_page_table_entry::MappingFlags as PageFaultFlags;

pub use crate::TrapFrame;

/// IRQ handler.
#[eii]
pub fn irq_handler(irq: usize) -> bool {
    trace!("IRQ {} triggered", irq);
    false
}

/// Page fault handler.
#[eii]
pub fn page_fault_handler(addr: VirtAddr, flags: PageFaultFlags) -> bool {
    warn!("Page fault at {:#x} with flags {:?}", addr, flags);
    false
}

/// Breakpoint handler.
///
/// The handler is invoked with a mutable reference to the trapped [`TrapFrame`]
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
/// infinite trap loop. The exact way to advance or modify the PC is
/// architecture-specific and depends on how [`TrapFrame`] encodes the saved
/// context.
#[eii]
pub fn breakpoint_handler(_tf: &mut TrapFrame) -> bool {
    false
}

/// Debug handler.
///
/// On `x86_64`, the handler is invoked for debug-related traps (for
/// example, hardware breakpoints, single-step traps, or other debug
/// exceptions). The handler receives a mutable reference to the trapped
/// [`TrapFrame`] and returns a boolean with the following meaning:
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
/// the PC.
#[cfg(target_arch = "x86_64")]
#[eii]
pub fn debug_handler(_tf: &mut TrapFrame) -> bool {
    false
}
