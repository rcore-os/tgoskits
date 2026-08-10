//! Synchronization policy for the ArceOS POSIX layer.

#[cfg(feature = "multitask")]
pub(crate) use ax_sync::Mutex;
#[cfg(not(feature = "multitask"))]
pub(crate) use ax_sync::SpinLock as Mutex;
