//! Register values captured before leaving the owning trap frame.

/// Privilege of an interrupted execution context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptedPrivilege {
    /// Less-privileged user execution.
    User,
    /// Privileged kernel execution.
    Kernel,
}

/// Value-only IRQ sampling input. It never retains a pointer into a trap stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptedContext {
    /// Saved instruction pointer.
    pub pc: usize,
    /// Stack pointer of the interrupted context.
    pub sp: usize,
    /// Saved frame pointer.
    pub fp: usize,
    /// Privilege of the interrupted context.
    pub privilege: InterruptedPrivilege,
}
