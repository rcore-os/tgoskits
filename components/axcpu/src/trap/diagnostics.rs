//! Runtime-owned diagnostics for CPU exception reports.

use core::fmt;

/// Saved machine registers, without any claim that their addresses are readable.
#[derive(Clone, Copy, Debug)]
pub struct BacktraceRegisters {
    /// Saved frame pointer.
    pub fp: usize,
    /// Interrupted instruction address.
    pub pc: usize,
    /// Saved return address, or zero on architectures without a link register.
    pub ra: usize,
}

impl BacktraceRegisters {
    pub(crate) const fn new(fp: usize, pc: usize, ra: usize) -> Self {
        Self { fp, pc, ra }
    }
}

/// Diagnostic service supplied once by the final runtime.
///
/// Calls can occur in fatal exception context with interrupts disabled. The
/// implementation must not block or assume the saved addresses are readable;
/// stack bounds and protected memory access belong to the runtime unwinder.
#[trait_ffi::def_extern_trait(mod_path = "trap::diagnostics")]
pub trait TrapDiagnostics {
    /// Formats a trap backtrace without retaining the formatter or stack memory.
    fn format_backtrace(
        registers: BacktraceRegisters,
        output: &mut fmt::Formatter<'_>,
    ) -> fmt::Result;
}

pub(crate) struct BacktraceDisplay(pub BacktraceRegisters);

impl fmt::Display for BacktraceDisplay {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        trap_diagnostics::format_backtrace(self.0, output)
    }
}
