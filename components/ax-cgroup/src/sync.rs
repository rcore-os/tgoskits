//! Synchronization selected by the consuming execution environment.

#[cfg(not(feature = "multitask"))]
pub(crate) use ax_kspin::SpinNoIrq as CgroupMutex;
#[cfg(feature = "multitask")]
pub(crate) use ax_sync::PiMutex as CgroupMutex;
