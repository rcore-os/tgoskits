use ax_memory_addr::VirtAddr;

use crate::{trap::PageFaultFlags, uspace::ExceptionInfo};

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
    #[cfg(target_arch = "x86_64")]
    /// A debug exception.
    Debug,
    /// A breakpoint exception.
    Breakpoint,
    /// An illegal instruction exception.
    IllegalInstruction,
    /// A misaligned access exception.
    Misaligned,
    /// Other kinds of exceptions.
    Other,
}
