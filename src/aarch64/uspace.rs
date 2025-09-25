//! Structures and functions for user space.

use core::ops::{Deref, DerefMut};

use aarch64_cpu::registers::ESR_EL1;
use memory_addr::VirtAddr;

use crate::TrapFrame;

pub use crate::uspace_common::{ExceptionKind, ReturnReason};

/// Context to enter user space.
pub struct UserContext(TrapFrame);

impl UserContext {
    /// Creates an empty context with all registers set to zero.
    pub const fn empty() -> Self {
        Self(TrapFrame::new())
    }

    /// Creates a new context with the given entry point, user stack pointer,
    /// and the argument.
    pub fn new(entry: usize, ustack_top: VirtAddr, arg0: usize) -> Self {
        use aarch64_cpu::registers::SPSR_EL1;
        let mut regs = [0; 31];
        regs[0] = arg0 as _;
        Self(TrapFrame {
            r: regs,
            usp: ustack_top.as_usize() as _,
            tpidr: 0,
            elr: entry as _,
            spsr: (SPSR_EL1::M::EL0t
                + SPSR_EL1::D::Masked
                + SPSR_EL1::A::Masked
                + SPSR_EL1::I::Unmasked
                + SPSR_EL1::F::Masked)
                .value,
        })
    }

    /// Creates a new context from the given [`TrapFrame`].
    pub const fn from(trap_frame: &TrapFrame) -> Self {
        Self(*trap_frame)
    }

    /// Enters user space.
    ///
    /// It restores the user registers and jumps to the user entry point
    /// (saved in `elr`).
    ///
    /// This function returns when an exception or syscall occurs.
    pub fn run(&mut self) -> ReturnReason {
        // TODO: implement
        ReturnReason::Unknown
    }
}

impl Deref for UserContext {
    type Target = TrapFrame;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for UserContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<TrapFrame> for UserContext {
    fn from(tf: TrapFrame) -> Self {
        Self(tf)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExceptionInfo {
    pub ec: ESR_EL1::EC::Value,
    pub il: bool,
    pub iss: u32,
}

impl ExceptionInfo {
    pub fn kind(&self) -> ExceptionKind {
        // TODO: implement
        ExceptionKind::Other
    }
}
