#[cfg(test)]
pub use spin::{Mutex as IrqMutex, Mutex as SleepMutex};
#[cfg(test)]
pub type IrqMutexGuard<'a, T> = spin::MutexGuard<'a, T, spin::Spin>;
#[cfg(test)]
pub type SleepMutexGuard<'a, T> = spin::MutexGuard<'a, T, spin::Spin>;

#[cfg(not(test))]
pub use ax_kspin::{SpinNoIrq as IrqMutex, SpinNoIrqGuard as IrqMutexGuard};
#[cfg(not(test))]
pub use ax_sync::{Mutex as SleepMutex, MutexGuard as SleepMutexGuard};
