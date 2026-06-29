#[cfg(not(test))]
pub use ax_kspin::{SpinNoIrq as IrqMutex, SpinNoIrqGuard as IrqMutexGuard};
#[cfg(test)]
pub use ax_kspin::{SpinRaw as IrqMutex, SpinRaw as SleepMutex};
#[cfg(test)]
pub use ax_kspin::{SpinRawGuard as IrqMutexGuard, SpinRawGuard as SleepMutexGuard};
#[cfg(not(test))]
pub use ax_sync::{Mutex as SleepMutex, MutexGuard as SleepMutexGuard};
