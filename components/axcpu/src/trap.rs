//! Trap handling.

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_memory_addr::VirtAddr;
pub use ax_page_table_entry::MappingFlags as PageFaultFlags;

pub use crate::TrapFrame;

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

/// IRQ handler.
pub fn irq_handler(irq: usize) -> bool {
    let handler = IRQ_HANDLER.load(Ordering::Acquire);
    let handler = if handler == 0 {
        default_irq_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `IrqHandler`.
        unsafe { core::mem::transmute::<usize, IrqHandler>(handler) }
    };
    handler(irq)
}

/// Page fault handler.
pub fn page_fault_handler(addr: VirtAddr, flags: PageFaultFlags) -> bool {
    let handler = PAGE_FAULT_HANDLER.load(Ordering::Acquire);
    let handler = if handler == 0 {
        default_page_fault_handler
    } else {
        // SAFETY: the atomic only stores function pointers of type `PageFaultHandler`.
        unsafe { core::mem::transmute::<usize, PageFaultHandler>(handler) }
    };
    handler(addr, flags)
}
