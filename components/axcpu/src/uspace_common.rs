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
    /// An integer arithmetic exception, i.e. x86 `#DE` (divide-by-zero or the
    /// `INT_MIN / -1` overflow). On x86 this is a real CPU trap that must become
    /// `SIGFPE`; the other architectures do not trap on integer divide-by-zero,
    /// so they never produce this kind.
    ArithmeticError,
    /// Other kinds of exceptions.
    Other,
}

/// Architecture-neutral syndrome fields for user-space exceptions.
///
/// The meaning of each field remains architecture-specific, but this shape
/// gives OS code a single way to log or forward the raw trap details without
/// reaching into every architecture's private register type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExceptionSyndrome {
    /// Raw syndrome/status register value when the architecture exposes one.
    pub raw: u64,
    /// Primary exception class or code.
    pub class: u64,
    /// Architecture-specific instruction syndrome or subcode.
    pub iss: u64,
}
