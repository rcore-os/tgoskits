//! Console input, output, and reversible boot-console ownership.

pub use ax_plat::console::{
    ConsoleDeviceId, ConsoleDeviceIdError, ConsoleDeviceIdResult, claim_runtime_output, device_id,
    read_bytes, write_bytes, write_text_bytes,
};
#[cfg(feature = "irq")]
pub use ax_plat::console::{ConsoleIrqEvent, handle_irq, irq_num, set_input_irq_enabled};

/// Failure to acquire exclusive suspension of the low-level boot console.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootConsoleOutputError;

impl core::fmt::Display for BootConsoleOutputError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("boot-console output is already suspended or cannot be suspended")
    }
}

impl core::error::Error for BootConsoleOutputError {}

/// A reversible suspension of low-level boot-console register access.
///
/// Dropping the lease restores boot-console output unless a runtime driver has
/// permanently claimed the same output path through [`claim_runtime_output`].
#[derive(Debug)]
pub struct BootConsoleOutputLease {
    active: bool,
}

impl Drop for BootConsoleOutputLease {
    fn drop(&mut self) {
        if self.active {
            ax_plat::console::resume_boot_output();
            self.active = false;
        }
    }
}

/// Suspends low-level boot-console output until the returned lease is dropped.
///
/// # Errors
///
/// Returns [`BootConsoleOutputError`] if another owner already suspended the
/// boot console or the selected platform cannot provide reversible handoff.
pub fn suspend_boot_output() -> Result<BootConsoleOutputLease, BootConsoleOutputError> {
    if !ax_plat::console::try_suspend_boot_output() {
        return Err(BootConsoleOutputError);
    }
    Ok(BootConsoleOutputLease { active: true })
}
