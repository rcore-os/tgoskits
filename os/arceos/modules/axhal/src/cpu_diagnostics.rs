use core::fmt;

use ax_cpu::trap::BacktraceRegisters;

struct RuntimeDiagnostics;

ax_cpu::trap::diagnostics::trap_diagnostics::impl_trait! {
    impl TrapDiagnostics for RuntimeDiagnostics {
        fn format_backtrace(registers: BacktraceRegisters, output: &mut fmt::Formatter<'_>) -> fmt::Result {
            let trace = axbacktrace::Backtrace::capture_trap(registers.fp, registers.pc, registers.ra);
            fmt::Display::fmt(&trace.kind("trap"), output)
        }
    }
}
