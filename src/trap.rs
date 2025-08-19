//! Trap handling.

use core::fmt::Debug;

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

#[cfg(feature = "uspace")]
pub enum ExceptionKind {
    Other,
    Breakpoint,
    IllegalInstruction,
    Misaligned,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ExceptionTableEntry {
    from: usize,
    to: usize,
}

pub fn fixup_exception(tf: &mut TrapFrame) -> bool {
    let ip = tf.ip();
    let entries = unsafe {
        core::slice::from_raw_parts(
            _ex_table_start as *const ExceptionTableEntry,
            (_ex_table_end as usize - _ex_table_start as usize)
                / core::mem::size_of::<ExceptionTableEntry>(),
        )
    };
    match entries.binary_search_by(|e| e.from.cmp(&ip)) {
        Ok(entry) => {
            tf.set_ip(entries[entry].to);
            true
        }
        Err(_) => false,
    }
}

pub(crate) fn init_exception_table() {
    // Sort exception table
    let ex_table = unsafe {
        core::slice::from_raw_parts_mut(
            _ex_table_start as *mut ExceptionTableEntry,
            (_ex_table_end as usize - _ex_table_start as usize) / size_of::<ExceptionTableEntry>(),
        )
    };
    ex_table.sort();
}

unsafe extern "C" {
    fn _ex_table_start();
    fn _ex_table_end();
}
