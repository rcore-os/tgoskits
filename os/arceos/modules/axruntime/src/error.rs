use ax_alloc::AllocError;
#[cfg(feature = "paging")]
use ax_hal::cache::TlbShootdownError;
use ax_hal::irq::IrqError;
#[cfg(feature = "paging")]
use ax_mm::MmError;
#[cfg(feature = "fs")]
use axfs_ng_vfs::VfsError;
#[cfg(feature = "paging")]
use axklib::KlibError;
use rdif_serial::ConfigError;
use crate::task::TaskError;

/// Errors owned by the ArceOS runtime layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeError {
    /// Address-space management failed while updating runtime mappings.
    #[cfg(feature = "paging")]
    #[error(transparent)]
    Mm(#[from] MmError),
    /// A cross-CPU TLB invalidation failed after a mapping update.
    #[cfg(feature = "paging")]
    #[error(transparent)]
    TlbShootdown(#[from] TlbShootdownError),
    /// Interrupt discovery or registration failed.
    #[error(transparent)]
    Irq(#[from] IrqError),
    /// Runtime-owned storage allocation failed.
    #[error(transparent)]
    Allocation(#[from] AllocError),
    /// A filesystem operation used by a runtime adapter failed.
    #[cfg(feature = "fs")]
    #[error(transparent)]
    Vfs(#[from] VfsError),
    /// The scheduler rejected a runtime-owned task operation.
    #[error(transparent)]
    Task(#[from] TaskError),
    /// A UART rejected its requested configuration.
    #[error(transparent)]
    SerialConfig(#[from] ConfigError),
    /// A platform console ownership transition was requested out of order.
    #[error(transparent)]
    ConsoleHandoff(#[from] ax_hal::console::ConsoleHandoffError),
    /// Another serial runtime already owns console log routing.
    #[error("another serial runtime already owns console routing")]
    SerialConsoleBusy,
    /// The runtime console handoff failed after early ownership was revoked.
    #[error("runtime console failed closed")]
    ConsoleFailedClosed,
    /// A serial operation requires a running port.
    #[error("serial runtime is not started")]
    SerialNotStarted,
    /// The bounded serial control queue is full.
    #[error("serial control queue is busy")]
    SerialControlBusy,
    /// A bounded runtime queue has no capacity without waiting.
    #[error("runtime operation would block")]
    WouldBlock,
    /// The selected runtime capability does not support the operation.
    #[error("runtime operation is not supported")]
    OperationNotSupported,
    /// A CPU index is outside the runtime's online CPU set.
    #[error("invalid runtime CPU index {cpu}")]
    InvalidCpu { cpu: usize },
}

/// A result returned by an ArceOS runtime-owned operation.
pub type RuntimeResult<T = ()> = Result<T, RuntimeError>;

/// Adapt a runtime-domain error at the external kernel capability boundary.
#[cfg(feature = "paging")]
pub(crate) fn runtime_error_to_klib_error(error: RuntimeError) -> KlibError {
    match error {
        #[cfg(feature = "paging")]
        RuntimeError::Mm(error) => match error {
            MmError::InvalidInput(_) => KlibError::InvalidInput,
            MmError::NoMemory => KlibError::NoMemory,
            MmError::AlreadyExists => KlibError::AlreadyExists,
            MmError::BadAddress => KlibError::BadAddress,
            MmError::BadState(_) => KlibError::BadState,
            MmError::Unsupported => KlibError::Unsupported,
        },
        #[cfg(feature = "paging")]
        RuntimeError::TlbShootdown(error) => match error {
            TlbShootdownError::CpuOffline | TlbShootdownError::Unsupported => {
                KlibError::Unsupported
            }
            TlbShootdownError::Timeout => KlibError::TimedOut,
            TlbShootdownError::Platform => KlibError::Io,
        },
        RuntimeError::Irq(error) => match error {
            IrqError::InvalidIrq | IrqError::InvalidCpu => KlibError::InvalidInput,
            IrqError::CpuOffline | IrqError::Unsupported => KlibError::Unsupported,
            IrqError::Timeout => KlibError::TimedOut,
            IrqError::Busy | IrqError::InIrqContext => KlibError::ResourceBusy,
            IrqError::NoMemory => KlibError::NoMemory,
            IrqError::NotFound => KlibError::NotFound,
            IrqError::Controller => KlibError::Io,
        },
        RuntimeError::Allocation(_) => KlibError::NoMemory,
        #[cfg(feature = "fs")]
        RuntimeError::Vfs(error) => match error {
            VfsError::AlreadyExists => KlibError::AlreadyExists,
            VfsError::BadAddress => KlibError::BadAddress,
            VfsError::NoMemory => KlibError::NoMemory,
            VfsError::ResourceBusy => KlibError::ResourceBusy,
            VfsError::TimedOut => KlibError::TimedOut,
            VfsError::Unsupported | VfsError::OperationNotSupported => KlibError::Unsupported,
            _ => KlibError::Io,
        },
        RuntimeError::Task(error) => task_error_to_klib_error(error),
        RuntimeError::SerialConfig(error) => match error {
            ConfigError::InvalidBaudrate
            | ConfigError::UnsupportedDataBits
            | ConfigError::UnsupportedStopBits
            | ConfigError::UnsupportedParity => KlibError::InvalidInput,
            ConfigError::Timeout => KlibError::TimedOut,
            ConfigError::RegisterError => KlibError::Io,
        },
        RuntimeError::ConsoleHandoff(_) => KlibError::BadState,
        RuntimeError::SerialConsoleBusy => KlibError::ResourceBusy,
        RuntimeError::ConsoleFailedClosed => KlibError::BadState,
        RuntimeError::SerialNotStarted => KlibError::BadState,
        RuntimeError::SerialControlBusy => KlibError::ResourceBusy,
        RuntimeError::WouldBlock => KlibError::ResourceBusy,
        RuntimeError::OperationNotSupported => KlibError::Unsupported,
        RuntimeError::InvalidCpu { .. } => KlibError::InvalidInput,
    }
}

#[cfg(feature = "paging")]
fn task_error_to_klib_error(error: TaskError) -> KlibError {
    use ax_task::runtime::RuntimeStatus;

    match error {
        TaskError::InvalidConfiguration
        | TaskError::InvalidCpuCount(_)
        | TaskError::InvalidCpu(_)
        | TaskError::InvalidNice(_)
        | TaskError::InvalidRtPriority(_)
        | TaskError::InvalidRoundRobinQuantum
        | TaskError::InvalidDeadline { .. }
        | TaskError::UnsupportedDeadlineFlags(_) => KlibError::InvalidInput,
        TaskError::TimerCapacity | TaskError::ThreadCapacity => KlibError::NoMemory,
        TaskError::RuntimeFailure(status) if status == RuntimeStatus::NoMemory as u32 => {
            KlibError::NoMemory
        }
        TaskError::CpuOffline(_)
        | TaskError::CpuNotQuiescent(_)
        | TaskError::LastOnlineCpu(_)
        | TaskError::DeadlineAdmission
        | TaskError::DeadlineAffinity
        | TaskError::ActiveTimerAffinity
        | TaskError::ThreadBusy
        | TaskError::CpuOwnerBorrowed => KlibError::ResourceBusy,
        TaskError::StaleThreadId => KlibError::NotFound,
        TaskError::UnsafeContext
        | TaskError::NotInitialized
        | TaskError::InvalidRuntimeHandle
        | TaskError::CpuOwnerMismatch { .. }
        | TaskError::ExecutorOwnerMismatch { .. }
        | TaskError::CpuAlreadyOnline(_)
        | TaskError::InvalidTransition { .. }
        | TaskError::AlreadyQueued
        | TaskError::NotReady
        | TaskError::NotExited
        | TaskError::NoRunnableThread
        | TaskError::InvalidPiState
        | TaskError::InvalidPiWaitState(_)
        | TaskError::PiCycle
        | TaskError::PiChainLimit { .. }
        | TaskError::RuntimeFailure(_) => KlibError::BadState,
    }
}
