use memory_addr::VirtAddr;

use crate::{trap::PageFaultFlags, uspace::ExceptionInfo, TrapFrame};

/// A reason as to why the control of the CPU is returned from
/// the user space to the kernel.
#[derive(Debug, Clone, Copy)]
pub enum ReturnReason {
    /// An interrupt.
    Interrupt,
    /// A system call.
    Syscall,
    /// A page fault.
    PageFault(VirtAddr, PageFaultFlags),
    /// Other kinds of exceptions.
    Exception(ExceptionInfo),
    /// Unknown reason.
    Unknown,
}

/// A generalized kind for [`ExceptionInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExceptionKind {
    /// A breakpoint exception.
    Breakpoint,
    /// An illegal instruction exception.
    IllegalInstruction,
    /// A misaligned access exception.
    Misaligned,
    /// Other kinds of exceptions.
    Other,
}

#[repr(C)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ExceptionTableEntry {
    from: usize,
    to: usize,
}

unsafe extern "C" {
    static _ex_table_start: [ExceptionTableEntry; 0];
    static _ex_table_end: [ExceptionTableEntry; 0];
}

impl TrapFrame {
    pub(crate) fn fixup_exception(&mut self) -> bool {
        let entries = unsafe {
            core::slice::from_raw_parts(
                _ex_table_start.as_ptr(),
                _ex_table_end
                    .as_ptr()
                    .offset_from_unsigned(_ex_table_start.as_ptr()),
            )
        };
        match entries.binary_search_by(|e| e.from.cmp(&self.ip())) {
            Ok(entry) => {
                self.set_ip(entries[entry].to);
                true
            }
            Err(_) => false,
        }
    }
}

pub(crate) fn init_exception_table() {
    // Sort exception table
    let ex_table = unsafe {
        core::slice::from_raw_parts_mut(
            _ex_table_start.as_ptr().cast_mut(),
            _ex_table_end
                .as_ptr()
                .offset_from_unsigned(_ex_table_start.as_ptr()),
        )
    };
    ex_table.sort_unstable();
}
