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
