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
