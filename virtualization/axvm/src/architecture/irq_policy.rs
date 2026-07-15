//! Host-testable validation of architecture-specific interrupt modes.

use axvm_types::VMInterruptMode;

use crate::{AxVmError, AxVmResult};

/// Rejects interrupt modes that have no complete backend on an architecture.
#[allow(
    dead_code,
    reason = "only architectures without a Hybrid backend call this policy"
)]
pub(crate) fn validate_irq_mode(
    architecture: &'static str,
    supports_hybrid: bool,
    mode: VMInterruptMode,
) -> AxVmResult {
    if !supports_hybrid && mode == VMInterruptMode::Hybrid {
        return Err(AxVmError::Unsupported {
            operation: "configure VM interrupts",
            detail: alloc::format!(
                "interrupt_mode=hybrid has no {architecture} forwarding backend"
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axvm_types::VMInterruptMode;

    use super::validate_irq_mode;

    #[test]
    fn riscv_hybrid_is_rejected_without_changing_passthrough() {
        assert!(validate_irq_mode("RISC-V", false, VMInterruptMode::Hybrid).is_err());
        assert!(validate_irq_mode("RISC-V", false, VMInterruptMode::Passthrough).is_ok());
    }
}
