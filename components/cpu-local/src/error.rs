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
    /// The selected current-context source does not identify a valid context.
    #[error("current-context source does not identify this CPU's execution context")]
    CurrentContextMismatch,
}

/// Failure while preparing or completing an execution-context switch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ContextSwitchError {
    /// CPU-local state could not be validated.
    #[error(transparent)]
    CpuLocal(#[from] CpuLocalError),
    /// The outgoing header is not the context currently selected on this CPU.
    #[error("outgoing context does not match the selected current context")]
    CurrentContextMismatch,
    /// The switch tail was paired with another previous context.
    #[error("previous-context token does not match the supplied context header")]
    PreviousContextMismatch,
    /// The next context is already running or is in another binding transition.
    #[error("next context is already bound to a CPU")]
    NextContextAlreadyBound,
    /// The incoming switch tail attempted to consume an obsolete binding epoch.
    #[error("previous-context binding epoch is stale")]
    StalePreviousBinding,
}
