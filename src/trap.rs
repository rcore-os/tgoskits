//! Trap handling.

use memory_addr::VirtAddr;

pub use crate::TrapFrame;
pub use linkme::distributed_slice as def_trap_handler;
pub use linkme::distributed_slice as register_trap_handler;
pub use page_table_entry::MappingFlags as PageFaultFlags;

/// A slice of IRQ handler functions.
#[def_trap_handler]
pub static IRQ: [fn(usize) -> bool];

/// A slice of page fault handler functions.
#[def_trap_handler]
pub static PAGE_FAULT: [fn(VirtAddr, PageFaultFlags) -> bool];

#[allow(unused_macros)]
macro_rules! handle_trap {
    ($trap:ident, $($args:tt)*) => {{
        let mut iter = $crate::trap::$trap.iter();
        if let Some(func) = iter.next() {
            if iter.next().is_some() {
                warn!("Multiple handlers for trap {} are not currently supported", stringify!($trap));
            }
            func($($args)*)
        } else {
            warn!("No registered handler for trap {}", stringify!($trap));
            false
        }
    }}
}

#[cfg(feature = "uspace")]
#[derive(Debug, Clone, Copy)]
pub enum ReturnReason {
    Unknown,
    Interrupt,
    Syscall,
    PageFault(VirtAddr, PageFaultFlags),
    Exception(crate::uspace::ExceptionInfo),
}
