//! Synchronization selected by the consuming execution environment.

pub(crate) use ax_sync::SpinLock as CgroupMutex;
