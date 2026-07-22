/// Failure to construct, install, or observe CPU-local state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CpuLocalError {
    /// No runtime CPU area has been installed in the architecture register.
    #[error("CPU-local area is not installed")]
    AreaNotInstalled,
    /// A runtime CPU-area address is null or does not meet prefix alignment.
    #[error("CPU-local area base {base:#x} is null or misaligned")]
    InvalidAreaBase {
        /// Rejected runtime address.
        base: usize,
    },
    /// Address arithmetic for the fixed CPU-area prefix overflowed.
    #[error("CPU-local prefix address calculation overflowed")]
    AddressOverflow,
    /// The immutable area header does not describe its actual address.
    #[error("CPU-local area header does not match its runtime address")]
    AreaIdentityMismatch,
    /// The kernel is running at an exception level unsupported by this backend.
    #[error("CPU-local registers do not support host exception level {level}")]
    UnsupportedHostLevel {
        /// Architecture-specific live exception level.
        level: usize,
    },
    /// The CPU runtime slot and architecture current-thread register disagree.
    #[error("current-thread register and CPU runtime slot disagree")]
    CurrentThreadMismatch,
}

/// Failure while preparing or completing a scheduler thread switch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ThreadSwitchError {
    /// CPU-local state could not be validated.
    #[error(transparent)]
    CpuLocal(#[from] CpuLocalError),
    /// The outgoing header is not the thread currently published on this CPU.
    #[error("outgoing task does not match the published current thread")]
    CurrentThreadMismatch,
    /// The incoming switch tail was paired with another previous task.
    #[error("previous-task token does not match the supplied task header")]
    PreviousThreadMismatch,
    /// The next task is already running or is in another binding transition.
    #[error("next task is already bound to a CPU")]
    NextThreadAlreadyBound,
    /// The incoming switch tail attempted to consume an obsolete binding epoch.
    #[error("previous-task binding epoch is stale")]
    StalePreviousBinding,
}
