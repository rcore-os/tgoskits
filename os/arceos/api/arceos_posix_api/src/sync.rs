//! Synchronization policy for the ArceOS POSIX layer.

#[cfg(feature = "multitask")]
pub(crate) use ax_runtime::sync::Mutex;
#[cfg(not(feature = "multitask"))]
pub(crate) use ax_runtime::sync::SpinLock as Mutex;
