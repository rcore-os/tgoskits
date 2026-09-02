use ax_alloc::AllocError;
use ax_cgroup::CgroupError;
use ax_fs_ng::BlockError;
use ax_hal::{cache::TlbShootdownError, paging::PagingError};
use ax_io::IoError;
use ax_memory_set::MappingError;
use ax_mm::MmError;
use ax_net::NetError;
use ax_runtime::{RuntimeError, serial::ConfigError};
use ax_task::future::{Elapsed, Interrupted, PollIoError, TaskError};
use axfs_ng_vfs::VfsError;
use dma_api::DmaError;
#[cfg(all(test, not(axtest)))]
use rdif_block::{BlkError, RequestOp};
#[cfg(feature = "sg2002")]
use sg2002_tpu::{ion::IonError, tpu::error::TpuError};
use starry_signal::SignalError;
use starry_vm::VmError;
use syscalls::Errno;

/// The operation that gives a DMA-domain failure its Linux ABI meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DmaOperation {
    /// Allocate storage for an exported or imported dma-buf.
    BufferAllocation,
    /// Program or execute a device DMA transaction.
    DeviceIo,
}

/// Errors owned by the Starry kernel below the Linux syscall ABI boundary.
#[derive(Debug, thiserror::Error)]
pub enum StarryError {
    #[error("Linux ABI operation failed: {0}")]
    Errno(Errno),
    #[error(transparent)]
    Vm(#[from] VmError),
    #[error(transparent)]
    Signal(#[from] SignalError),
    #[error(transparent)]
    Mm(#[from] MmError),
    #[error(transparent)]
    Vfs(#[from] VfsError),
    #[error(transparent)]
    Mapping(#[from] MappingError),
    #[error(transparent)]
    Paging(#[from] PagingError),
    #[error(transparent)]
    TlbShootdown(#[from] TlbShootdownError),
    #[error(transparent)]
    Alloc(#[from] AllocError),
    #[error(transparent)]
    Cgroup(#[from] CgroupError),
    #[error(transparent)]
    IoDomain(#[from] IoError),
    #[error(transparent)]
    Net(#[from] NetError),
    #[error(transparent)]
    Task(#[from] TaskError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Block(#[from] BlockError),
    #[error(transparent)]
    Driver(#[from] ax_driver::Error),
    #[cfg(feature = "sg2002")]
    #[error(transparent)]
    Ion(#[from] IonError),
    #[cfg(feature = "sg2002")]
    #[error(transparent)]
    Tpu(#[from] TpuError),
    #[error(transparent)]
    Format(#[from] core::fmt::Error),
    #[error(transparent)]
    TaskInterrupted(#[from] Interrupted),
    #[error(transparent)]
    TaskElapsed(#[from] Elapsed),
    #[error("DMA {operation:?} failed: {source}")]
    Dma {
        operation: DmaOperation,
        #[source]
        source: DmaError,
    },
    #[error("kernel object already exists")]
    AlreadyExists,
    #[error("argument list is too long")]
    ArgumentListTooLong,
    #[error("bad address")]
    BadAddress,
    #[error("bad file descriptor")]
    BadFileDescriptor,
    #[error("kernel object is in an invalid state")]
    BadState,
    #[error("broken pipe")]
    BrokenPipe,
    #[error("operation crosses devices")]
    CrossesDevices,
    #[error("filesystem traversal loop detected")]
    FilesystemLoop,
    #[error("illegal byte sequence")]
    IllegalBytes,
    #[error("operation is in progress")]
    InProgress,
    #[error("operation was interrupted")]
    Interrupted,
    #[error("invalid kernel data")]
    InvalidData,
    #[error("invalid executable image")]
    InvalidExecutable,
    #[error("malformed executable image")]
    MalformedExecutable,
    #[error("invalid kernel input")]
    InvalidInput,
    #[error("kernel I/O failed")]
    Io,
    #[error("object is a directory")]
    IsADirectory,
    #[error("name is too long")]
    NameTooLong,
    #[error("kernel allocation failed")]
    NoMemory,
    #[error("device does not exist")]
    NoSuchDevice,
    #[error("device or address does not exist")]
    NoSuchDeviceOrAddress,
    #[error("process does not exist")]
    NoSuchProcess,
    #[error("object is not a directory")]
    NotADirectory,
    #[error("object is not a socket")]
    NotASocket,
    #[error("object is not a tty")]
    NotATty,
    #[error("object was not found")]
    NotFound,
    #[error("operation is not permitted")]
    OperationNotPermitted,
    #[error("operation is not supported by this object")]
    OperationNotSupported,
    #[error("result is out of range")]
    OutOfRange,
    #[error("permission denied")]
    PermissionDenied,
    #[error("filesystem is read-only")]
    ReadOnlyFilesystem,
    #[error("kernel resource is busy")]
    ResourceBusy,
    #[error("storage is full")]
    StorageFull,
    #[error("operation timed out")]
    TimedOut,
    #[error("too many files are open")]
    TooManyOpenFiles,
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("kernel capability is unavailable")]
    Unsupported,
    #[error("operation would block")]
    WouldBlock,
    #[error("write made no progress")]
    WriteZero,
}

impl From<Errno> for StarryError {
    fn from(errno: Errno) -> Self {
        Self::Errno(errno)
    }
}

impl From<StarryError> for VfsError {
    fn from(error: StarryError) -> Self {
        match error {
            StarryError::Vfs(error) => error,
            StarryError::BadState => Self::BadState,
            StarryError::InvalidData => Self::InvalidData,
            error => vfs_error_from_errno(error.linux_errno()),
        }
    }
}

impl StarryError {
    /// Convert an internal domain failure to its Linux syscall ABI errno.
    pub fn linux_errno(&self) -> Errno {
        match self {
            Self::Errno(errno) => *errno,
            Self::Vm(error) => vm_errno(*error),
            Self::Signal(SignalError::UserMemory(error)) => vm_errno(*error),
            Self::Mm(error) => mm_errno(*error),
            Self::Vfs(error) => vfs_errno(*error),
            Self::Mapping(error) => mapping_errno(error),
            Self::Paging(error) => match error {
                PagingError::NoMemory => Errno::ENOMEM,
                _ => Errno::EINVAL,
            },
            Self::TlbShootdown(error) => tlb_errno(*error),
            Self::Alloc(error) => alloc_errno(*error),
            Self::Cgroup(error) => cgroup_errno(*error),
            Self::IoDomain(error) => io_errno(*error),
            Self::Net(error) => io_errno((*error).into()),
            Self::Task(error) => task_errno(*error),
            Self::Runtime(error) => runtime_errno(error),
            Self::Block(error) => block_errno(error),
            Self::Driver(_) => Errno::EIO,
            #[cfg(feature = "sg2002")]
            Self::Ion(error) => ion_errno(*error),
            #[cfg(feature = "sg2002")]
            Self::Tpu(error) => tpu_errno(*error),
            Self::Format(_) => Errno::EINVAL,
            Self::TaskInterrupted(_) => Errno::EINTR,
            Self::TaskElapsed(_) => Errno::ETIMEDOUT,
            Self::Dma { operation, source } => dma_errno(*operation, source),
            Self::AlreadyExists => Errno::EEXIST,
            Self::ArgumentListTooLong => Errno::E2BIG,
            Self::BadAddress | Self::BadState => Errno::EFAULT,
            Self::BadFileDescriptor => Errno::EBADF,
            Self::BrokenPipe => Errno::EPIPE,
            Self::CrossesDevices => Errno::EXDEV,
            Self::FilesystemLoop => Errno::ELOOP,
            Self::IllegalBytes => Errno::EILSEQ,
            Self::InProgress => Errno::EINPROGRESS,
            Self::Interrupted => Errno::EINTR,
            Self::InvalidData | Self::InvalidInput => Errno::EINVAL,
            Self::InvalidExecutable | Self::MalformedExecutable => Errno::ENOEXEC,
            Self::Io | Self::UnexpectedEof | Self::WriteZero => Errno::EIO,
            Self::IsADirectory => Errno::EISDIR,
            Self::NameTooLong => Errno::ENAMETOOLONG,
            Self::NoMemory => Errno::ENOMEM,
            Self::NoSuchDevice => Errno::ENODEV,
            Self::NoSuchDeviceOrAddress => Errno::ENXIO,
            Self::NoSuchProcess => Errno::ESRCH,
            Self::NotADirectory => Errno::ENOTDIR,
            Self::NotASocket => Errno::ENOTSOCK,
            Self::NotATty => Errno::ENOTTY,
            Self::NotFound => Errno::ENOENT,
            Self::OperationNotPermitted => Errno::EPERM,
            Self::OperationNotSupported => Errno::EOPNOTSUPP,
            Self::OutOfRange => Errno::ERANGE,
            Self::PermissionDenied => Errno::EACCES,
            Self::ReadOnlyFilesystem => Errno::EROFS,
            Self::ResourceBusy => Errno::EBUSY,
            Self::StorageFull => Errno::ENOSPC,
            Self::TimedOut => Errno::ETIMEDOUT,
            Self::TooManyOpenFiles => Errno::EMFILE,
            Self::Unsupported => Errno::ENOSYS,
            Self::WouldBlock => Errno::EAGAIN,
        }
    }
}

impl PollIoError for StarryError {
    fn is_would_block(&self) -> bool {
        self.linux_errno() == Errno::EAGAIN
    }

    fn interrupted(error: Interrupted) -> Self {
        error.into()
    }
}

fn vm_errno(error: VmError) -> Errno {
    match error {
        VmError::BadAddress | VmError::AccessDenied => Errno::EFAULT,
        VmError::TooLong => Errno::ENAMETOOLONG,
    }
}

fn mm_errno(error: MmError) -> Errno {
    match error {
        MmError::InvalidInput(_) => Errno::EINVAL,
        MmError::NoMemory => Errno::ENOMEM,
        MmError::AlreadyExists => Errno::EEXIST,
        MmError::BadAddress | MmError::BadState(_) => Errno::EFAULT,
        MmError::Unsupported => Errno::ENOSYS,
    }
}

fn mapping_errno(error: &MappingError) -> Errno {
    match error {
        MappingError::InvalidParam => Errno::EINVAL,
        MappingError::AlreadyExists => Errno::EEXIST,
        MappingError::BadState | MappingError::NeedsRepair => Errno::EFAULT,
    }
}

fn tlb_errno(error: TlbShootdownError) -> Errno {
    match error {
        TlbShootdownError::CpuOffline | TlbShootdownError::Unsupported => Errno::ENOSYS,
        TlbShootdownError::Timeout => Errno::ETIMEDOUT,
        TlbShootdownError::GenerationExhausted => Errno::EOVERFLOW,
        TlbShootdownError::Platform => Errno::EIO,
    }
}

fn alloc_errno(error: AllocError) -> Errno {
    match error {
        AllocError::NoMemory => Errno::ENOMEM,
        AllocError::NotFound => Errno::ENOENT,
        AllocError::NotInitialized | AllocError::AlreadyInitialized => Errno::EFAULT,
        AllocError::MemoryOverlap => Errno::EEXIST,
        AllocError::InvalidParam | AllocError::NotAllocated => Errno::EINVAL,
    }
}

fn cgroup_errno(error: CgroupError) -> Errno {
    match error {
        CgroupError::NotInitialized | CgroupError::InvalidInput => Errno::EINVAL,
        CgroupError::NotFound => Errno::ENOENT,
        CgroupError::AlreadyExists => Errno::EEXIST,
        CgroupError::ResourceBusy => Errno::EBUSY,
        CgroupError::LimitExceeded => Errno::EAGAIN,
        CgroupError::NoSuchProcess => Errno::ESRCH,
        CgroupError::DirectoryNotEmpty => Errno::ENOTEMPTY,
    }
}

fn task_errno(error: TaskError) -> Errno {
    match error {
        TaskError::Interrupted(_) => Errno::EINTR,
        TaskError::Elapsed(_) => Errno::ETIMEDOUT,
        TaskError::WouldBlock => Errno::EAGAIN,
        TaskError::Irq(_) => Errno::EIO,
    }
}

fn vfs_error_from_errno(errno: Errno) -> VfsError {
    match errno {
        Errno::EEXIST => VfsError::AlreadyExists,
        Errno::EFAULT => VfsError::BadAddress,
        Errno::EBADF => VfsError::BadFileDescriptor,
        Errno::EXDEV => VfsError::CrossesDevices,
        Errno::ENODATA => VfsError::DataMissing,
        Errno::ENOTEMPTY => VfsError::DirectoryNotEmpty,
        Errno::EUCLEAN => VfsError::FilesystemCorrupted,
        Errno::ELOOP => VfsError::FilesystemLoop,
        Errno::EFBIG => VfsError::FileTooLarge,
        Errno::EINVAL => VfsError::InvalidInput,
        Errno::EINTR => VfsError::Interrupted,
        Errno::EIO => VfsError::Io,
        Errno::EISDIR => VfsError::IsADirectory,
        Errno::ENAMETOOLONG => VfsError::NameTooLong,
        Errno::ENOMEM => VfsError::NoMemory,
        Errno::ENODEV => VfsError::NoSuchDevice,
        Errno::ENXIO => VfsError::NoSuchDeviceOrAddress,
        Errno::ENOTDIR => VfsError::NotADirectory,
        Errno::ENOTTY => VfsError::NotATty,
        Errno::ENOENT => VfsError::NotFound,
        Errno::EPERM => VfsError::OperationNotPermitted,
        Errno::EOPNOTSUPP => VfsError::OperationNotSupported,
        Errno::EACCES => VfsError::PermissionDenied,
        Errno::EDQUOT => VfsError::QuotaExceeded,
        Errno::EROFS => VfsError::ReadOnlyFilesystem,
        Errno::EBUSY => VfsError::ResourceBusy,
        Errno::ENOSPC => VfsError::StorageFull,
        Errno::ETIMEDOUT => VfsError::TimedOut,
        Errno::EMLINK => VfsError::TooManyLinks,
        Errno::ENOSYS => VfsError::Unsupported,
        Errno::EOVERFLOW => VfsError::ValueOverflow,
        Errno::EAGAIN => VfsError::WouldBlock,
        _ => VfsError::Io,
    }
}

fn runtime_errno(error: &RuntimeError) -> Errno {
    match error {
        RuntimeError::SerialConfig(error) => match error {
            ConfigError::InvalidBaudrate
            | ConfigError::UnsupportedDataBits
            | ConfigError::UnsupportedStopBits
            | ConfigError::UnsupportedParity => Errno::EINVAL,
            ConfigError::Timeout => Errno::ETIMEDOUT,
            ConfigError::RegisterError => Errno::EIO,
        },
        RuntimeError::SerialNotStarted => Errno::EFAULT,
        RuntimeError::SerialControlBusy => Errno::EBUSY,
        RuntimeError::WouldBlock => Errno::EAGAIN,
        RuntimeError::OperationNotSupported => Errno::EOPNOTSUPP,
        RuntimeError::InvalidCpu { .. } => Errno::EINVAL,
        _ => Errno::EIO,
    }
}

fn block_errno(error: &BlockError) -> Errno {
    match error {
        BlockError::InvalidRequest => Errno::EINVAL,
        BlockError::InvalidState | BlockError::RuntimeUnavailable => Errno::EFAULT,
        BlockError::WouldBlock => Errno::EAGAIN,
        BlockError::NoMemory => Errno::ENOMEM,
        BlockError::Unsupported => Errno::ENOSYS,
        BlockError::TimedOut => Errno::ETIMEDOUT,
        BlockError::ResourceBusy => Errno::EBUSY,
        BlockError::NotFound => Errno::ENOENT,
        BlockError::Io | BlockError::Irq(_) => Errno::EIO,
        BlockError::Device { source, .. } => block_errno(&BlockError::from(*source)),
    }
}

#[cfg(feature = "sg2002")]
fn ion_errno(error: IonError) -> Errno {
    match error {
        IonError::InvalidArg => Errno::EINVAL,
        IonError::NoMemory => Errno::ENOMEM,
        IonError::InvalidBuffer | IonError::BufferNotFound => Errno::ENOENT,
        IonError::BufferExists => Errno::EEXIST,
        IonError::InvalidHeap | IonError::NotSupported => Errno::ENOSYS,
        IonError::Internal => Errno::EINTR,
    }
}

#[cfg(feature = "sg2002")]
fn tpu_errno(error: TpuError) -> Errno {
    match error {
        TpuError::Timeout => Errno::ETIMEDOUT,
        TpuError::InvalidDmabuf | TpuError::PmuBufferNotAligned | TpuError::DmabufNotAligned => {
            Errno::EINVAL
        }
        TpuError::TdmaError(_) | TpuError::TiuError(_) => Errno::EIO,
        TpuError::NotInitialized => Errno::ENODEV,
        TpuError::Busy => Errno::EBUSY,
        TpuError::Interrupted => Errno::EINTR,
    }
}

fn dma_errno(operation: DmaOperation, source: &DmaError) -> Errno {
    match operation {
        DmaOperation::BufferAllocation => match source {
            DmaError::LayoutError(_) => Errno::EINVAL,
            _ => Errno::ENOMEM,
        },
        DmaOperation::DeviceIo => match source {
            DmaError::NoMemory => Errno::ENOMEM,
            DmaError::LayoutError(_) | DmaError::NullPointer | DmaError::ZeroSizedBuffer => {
                Errno::EINVAL
            }
            _ => Errno::EIO,
        },
    }
}

fn io_errno(error: IoError) -> Errno {
    match error {
        IoError::AddrInUse => Errno::EADDRINUSE,
        IoError::AlreadyConnected => Errno::EISCONN,
        IoError::AddressFamilyUnsupported => Errno::EAFNOSUPPORT,
        IoError::AlreadyExists => Errno::EEXIST,
        IoError::ArgumentListTooLong => Errno::E2BIG,
        IoError::BadAddress | IoError::BadState => Errno::EFAULT,
        IoError::BadFileDescriptor => Errno::EBADF,
        IoError::BrokenPipe => Errno::EPIPE,
        IoError::ConnectionRefused => Errno::ECONNREFUSED,
        IoError::ConnectionReset => Errno::ECONNRESET,
        IoError::CrossesDevices => Errno::EXDEV,
        IoError::DirectoryNotEmpty => Errno::ENOTEMPTY,
        IoError::DestAddrRequired => Errno::EDESTADDRREQ,
        IoError::FilesystemLoop => Errno::ELOOP,
        IoError::FileTooLarge => Errno::EFBIG,
        IoError::IllegalBytes => Errno::EILSEQ,
        IoError::InProgress => Errno::EINPROGRESS,
        IoError::Interrupted => Errno::EINTR,
        IoError::InvalidData | IoError::InvalidInput => Errno::EINVAL,
        IoError::InvalidExecutable => Errno::ENOEXEC,
        IoError::Io | IoError::UnexpectedEof | IoError::WriteZero => Errno::EIO,
        IoError::IsADirectory => Errno::EISDIR,
        IoError::MessageTooLong => Errno::EMSGSIZE,
        IoError::NameTooLong => Errno::ENAMETOOLONG,
        IoError::NoMemory => Errno::ENOMEM,
        IoError::NoSuchDevice => Errno::ENODEV,
        IoError::NoSuchDeviceOrAddress => Errno::ENXIO,
        IoError::NoSuchProcess => Errno::ESRCH,
        IoError::NotADirectory => Errno::ENOTDIR,
        IoError::NotASocket => Errno::ENOTSOCK,
        IoError::NotATty => Errno::ENOTTY,
        IoError::NotConnected => Errno::ENOTCONN,
        IoError::NotFound => Errno::ENOENT,
        IoError::OperationNotPermitted => Errno::EPERM,
        IoError::OperationNotSupported => Errno::EOPNOTSUPP,
        IoError::OutOfRange => Errno::ERANGE,
        IoError::PermissionDenied => Errno::EACCES,
        IoError::ProtocolOptionUnsupported => Errno::ENOPROTOOPT,
        IoError::ReadOnlyFilesystem => Errno::EROFS,
        IoError::ResourceBusy => Errno::EBUSY,
        IoError::StorageFull => Errno::ENOSPC,
        IoError::TimedOut => Errno::ETIMEDOUT,
        IoError::TooManyOpenFiles => Errno::EMFILE,
        IoError::Unsupported => Errno::ENOSYS,
        IoError::WouldBlock => Errno::EAGAIN,
    }
}

fn vfs_errno(error: VfsError) -> Errno {
    match error {
        VfsError::AlreadyExists => Errno::EEXIST,
        VfsError::BadAddress | VfsError::BadState => Errno::EFAULT,
        VfsError::BadFileDescriptor => Errno::EBADF,
        VfsError::CrossesDevices => Errno::EXDEV,
        VfsError::DataMissing => Errno::ENODATA,
        VfsError::DirectoryNotEmpty => Errno::ENOTEMPTY,
        VfsError::FilesystemCorrupted => Errno::EUCLEAN,
        VfsError::FilesystemLoop => Errno::ELOOP,
        VfsError::FileTooLarge => Errno::EFBIG,
        VfsError::InvalidData | VfsError::InvalidInput => Errno::EINVAL,
        VfsError::Interrupted => Errno::EINTR,
        VfsError::Io => Errno::EIO,
        VfsError::IsADirectory => Errno::EISDIR,
        VfsError::NameTooLong => Errno::ENAMETOOLONG,
        VfsError::NoMemory => Errno::ENOMEM,
        VfsError::NoSuchDevice => Errno::ENODEV,
        VfsError::NoSuchDeviceOrAddress => Errno::ENXIO,
        VfsError::NotADirectory => Errno::ENOTDIR,
        VfsError::NotATty => Errno::ENOTTY,
        VfsError::NotFound => Errno::ENOENT,
        VfsError::OperationNotPermitted => Errno::EPERM,
        VfsError::OperationNotSupported => Errno::EOPNOTSUPP,
        VfsError::PermissionDenied => Errno::EACCES,
        VfsError::QuotaExceeded => Errno::EDQUOT,
        VfsError::ReadOnlyFilesystem => Errno::EROFS,
        VfsError::ResourceBusy => Errno::EBUSY,
        VfsError::StorageFull => Errno::ENOSPC,
        VfsError::TimedOut => Errno::ETIMEDOUT,
        VfsError::TooManyLinks => Errno::EMLINK,
        VfsError::Unsupported => Errno::ENOSYS,
        VfsError::ValueOverflow => Errno::EOVERFLOW,
        VfsError::WouldBlock => Errno::EAGAIN,
    }
}

#[cfg(all(test, not(axtest)))]
fn errno_cases_hold<const N: usize>(cases: [(StarryError, Errno); N]) -> bool {
    cases
        .into_iter()
        .all(|(error, expected)| error.linux_errno() == expected)
}

#[cfg(all(test, not(axtest)))]
fn memory_errno_mappings_hold() -> bool {
    errno_cases_hold([
        (VmError::BadAddress.into(), Errno::EFAULT),
        (VmError::AccessDenied.into(), Errno::EFAULT),
        (VmError::TooLong.into(), Errno::ENAMETOOLONG),
        (
            SignalError::UserMemory(VmError::BadAddress).into(),
            Errno::EFAULT,
        ),
        (MmError::InvalidInput("test").into(), Errno::EINVAL),
        (MmError::NoMemory.into(), Errno::ENOMEM),
        (MmError::AlreadyExists.into(), Errno::EEXIST),
        (MmError::BadAddress.into(), Errno::EFAULT),
        (MmError::BadState("test").into(), Errno::EFAULT),
        (MmError::Unsupported.into(), Errno::ENOSYS),
        (MappingError::InvalidParam.into(), Errno::EINVAL),
        (MappingError::AlreadyExists.into(), Errno::EEXIST),
        (MappingError::BadState.into(), Errno::EFAULT),
        (PagingError::NoMemory.into(), Errno::ENOMEM),
        (PagingError::not_mapped().into(), Errno::EINVAL),
        (TlbShootdownError::CpuOffline.into(), Errno::ENOSYS),
        (TlbShootdownError::Timeout.into(), Errno::ETIMEDOUT),
        (TlbShootdownError::Unsupported.into(), Errno::ENOSYS),
        (TlbShootdownError::Platform.into(), Errno::EIO),
        (
            TlbShootdownError::GenerationExhausted.into(),
            Errno::EOVERFLOW,
        ),
        (AllocError::InvalidParam.into(), Errno::EINVAL),
        (AllocError::AlreadyInitialized.into(), Errno::EFAULT),
        (AllocError::MemoryOverlap.into(), Errno::EEXIST),
        (AllocError::NoMemory.into(), Errno::ENOMEM),
        (AllocError::NotAllocated.into(), Errno::EINVAL),
        (AllocError::NotInitialized.into(), Errno::EFAULT),
        (AllocError::NotFound.into(), Errno::ENOENT),
        (CgroupError::NotInitialized.into(), Errno::EINVAL),
        (CgroupError::NotFound.into(), Errno::ENOENT),
        (CgroupError::AlreadyExists.into(), Errno::EEXIST),
        (CgroupError::ResourceBusy.into(), Errno::EBUSY),
        (CgroupError::LimitExceeded.into(), Errno::EAGAIN),
        (CgroupError::InvalidInput.into(), Errno::EINVAL),
        (CgroupError::NoSuchProcess.into(), Errno::ESRCH),
        (CgroupError::DirectoryNotEmpty.into(), Errno::ENOTEMPTY),
    ])
}

#[cfg(all(test, not(axtest)))]
fn vfs_errno_mappings_hold() -> bool {
    errno_cases_hold([
        (VfsError::AlreadyExists.into(), Errno::EEXIST),
        (VfsError::BadAddress.into(), Errno::EFAULT),
        (VfsError::BadFileDescriptor.into(), Errno::EBADF),
        (VfsError::BadState.into(), Errno::EFAULT),
        (VfsError::CrossesDevices.into(), Errno::EXDEV),
        (VfsError::DataMissing.into(), Errno::ENODATA),
        (VfsError::DirectoryNotEmpty.into(), Errno::ENOTEMPTY),
        (VfsError::FilesystemCorrupted.into(), Errno::EUCLEAN),
        (VfsError::FilesystemLoop.into(), Errno::ELOOP),
        (VfsError::FileTooLarge.into(), Errno::EFBIG),
        (VfsError::InvalidData.into(), Errno::EINVAL),
        (VfsError::InvalidInput.into(), Errno::EINVAL),
        (VfsError::Interrupted.into(), Errno::EINTR),
        (VfsError::Io.into(), Errno::EIO),
        (VfsError::IsADirectory.into(), Errno::EISDIR),
        (VfsError::NameTooLong.into(), Errno::ENAMETOOLONG),
        (VfsError::NoMemory.into(), Errno::ENOMEM),
        (VfsError::NoSuchDevice.into(), Errno::ENODEV),
        (VfsError::NoSuchDeviceOrAddress.into(), Errno::ENXIO),
        (VfsError::NotADirectory.into(), Errno::ENOTDIR),
        (VfsError::NotATty.into(), Errno::ENOTTY),
        (VfsError::NotFound.into(), Errno::ENOENT),
        (VfsError::OperationNotPermitted.into(), Errno::EPERM),
        (VfsError::OperationNotSupported.into(), Errno::EOPNOTSUPP),
        (VfsError::PermissionDenied.into(), Errno::EACCES),
        (VfsError::QuotaExceeded.into(), Errno::EDQUOT),
        (VfsError::ReadOnlyFilesystem.into(), Errno::EROFS),
        (VfsError::ResourceBusy.into(), Errno::EBUSY),
        (VfsError::StorageFull.into(), Errno::ENOSPC),
        (VfsError::TimedOut.into(), Errno::ETIMEDOUT),
        (VfsError::TooManyLinks.into(), Errno::EMLINK),
        (VfsError::Unsupported.into(), Errno::ENOSYS),
        (VfsError::ValueOverflow.into(), Errno::EOVERFLOW),
        (VfsError::WouldBlock.into(), Errno::EAGAIN),
    ])
}

#[cfg(all(test, not(axtest)))]
fn io_errno_mappings_hold() -> bool {
    errno_cases_hold([
        (IoError::AddrInUse.into(), Errno::EADDRINUSE),
        (IoError::AlreadyConnected.into(), Errno::EISCONN),
        (
            IoError::AddressFamilyUnsupported.into(),
            Errno::EAFNOSUPPORT,
        ),
        (IoError::AlreadyExists.into(), Errno::EEXIST),
        (IoError::ArgumentListTooLong.into(), Errno::E2BIG),
        (IoError::BadAddress.into(), Errno::EFAULT),
        (IoError::BadFileDescriptor.into(), Errno::EBADF),
        (IoError::BadState.into(), Errno::EFAULT),
        (IoError::BrokenPipe.into(), Errno::EPIPE),
        (IoError::ConnectionRefused.into(), Errno::ECONNREFUSED),
        (IoError::ConnectionReset.into(), Errno::ECONNRESET),
        (IoError::CrossesDevices.into(), Errno::EXDEV),
        (IoError::DirectoryNotEmpty.into(), Errno::ENOTEMPTY),
        (IoError::DestAddrRequired.into(), Errno::EDESTADDRREQ),
        (IoError::FilesystemLoop.into(), Errno::ELOOP),
        (IoError::FileTooLarge.into(), Errno::EFBIG),
        (IoError::IllegalBytes.into(), Errno::EILSEQ),
        (IoError::InProgress.into(), Errno::EINPROGRESS),
        (IoError::Interrupted.into(), Errno::EINTR),
        (IoError::InvalidData.into(), Errno::EINVAL),
        (IoError::InvalidExecutable.into(), Errno::ENOEXEC),
        (IoError::InvalidInput.into(), Errno::EINVAL),
        (IoError::Io.into(), Errno::EIO),
        (IoError::IsADirectory.into(), Errno::EISDIR),
        (IoError::NameTooLong.into(), Errno::ENAMETOOLONG),
        (IoError::MessageTooLong.into(), Errno::EMSGSIZE),
        (IoError::NoMemory.into(), Errno::ENOMEM),
        (IoError::NoSuchDevice.into(), Errno::ENODEV),
        (IoError::NoSuchDeviceOrAddress.into(), Errno::ENXIO),
        (IoError::NoSuchProcess.into(), Errno::ESRCH),
        (IoError::NotADirectory.into(), Errno::ENOTDIR),
        (IoError::NotASocket.into(), Errno::ENOTSOCK),
        (IoError::NotATty.into(), Errno::ENOTTY),
        (IoError::NotConnected.into(), Errno::ENOTCONN),
        (IoError::NotFound.into(), Errno::ENOENT),
        (IoError::OperationNotPermitted.into(), Errno::EPERM),
        (IoError::OperationNotSupported.into(), Errno::EOPNOTSUPP),
        (IoError::OutOfRange.into(), Errno::ERANGE),
        (IoError::PermissionDenied.into(), Errno::EACCES),
        (
            IoError::ProtocolOptionUnsupported.into(),
            Errno::ENOPROTOOPT,
        ),
        (IoError::ReadOnlyFilesystem.into(), Errno::EROFS),
        (IoError::ResourceBusy.into(), Errno::EBUSY),
        (IoError::StorageFull.into(), Errno::ENOSPC),
        (IoError::TimedOut.into(), Errno::ETIMEDOUT),
        (IoError::TooManyOpenFiles.into(), Errno::EMFILE),
        (IoError::UnexpectedEof.into(), Errno::EIO),
        (IoError::Unsupported.into(), Errno::ENOSYS),
        (IoError::WouldBlock.into(), Errno::EAGAIN),
        (IoError::WriteZero.into(), Errno::EIO),
    ])
}

#[cfg(all(test, not(axtest)))]
fn block_errno_mappings_hold() -> bool {
    let device_error = |source| {
        StarryError::from(BlockError::Device {
            stage: "submit",
            operation: RequestOp::Read,
            lba: 9,
            source,
        })
    };

    errno_cases_hold([
        (BlockError::InvalidRequest.into(), Errno::EINVAL),
        (BlockError::InvalidState.into(), Errno::EFAULT),
        (BlockError::RuntimeUnavailable.into(), Errno::EFAULT),
        (BlockError::WouldBlock.into(), Errno::EAGAIN),
        (BlockError::NoMemory.into(), Errno::ENOMEM),
        (BlockError::Unsupported.into(), Errno::ENOSYS),
        (BlockError::TimedOut.into(), Errno::ETIMEDOUT),
        (BlockError::Io.into(), Errno::EIO),
        (BlockError::ResourceBusy.into(), Errno::EBUSY),
        (BlockError::NotFound.into(), Errno::ENOENT),
        (device_error(BlkError::NotSupported), Errno::ENOSYS),
        (device_error(BlkError::Retry), Errno::EAGAIN),
        (device_error(BlkError::NoMemory), Errno::ENOMEM),
        (device_error(BlkError::InvalidBlockIndex(9)), Errno::EINVAL),
        (device_error(BlkError::InvalidRequest), Errno::EINVAL),
        (device_error(BlkError::TimedOut), Errno::ETIMEDOUT),
        (device_error(BlkError::Io), Errno::EIO),
        (device_error(BlkError::Other("device failure")), Errno::EIO),
    ])
}

#[cfg(all(test, not(axtest)))]
fn leaf_errno_mappings_hold() -> bool {
    errno_cases_hold([
        (StarryError::AlreadyExists, Errno::EEXIST),
        (StarryError::ArgumentListTooLong, Errno::E2BIG),
        (StarryError::BadAddress, Errno::EFAULT),
        (StarryError::BadFileDescriptor, Errno::EBADF),
        (StarryError::BadState, Errno::EFAULT),
        (StarryError::BrokenPipe, Errno::EPIPE),
        (StarryError::CrossesDevices, Errno::EXDEV),
        (StarryError::FilesystemLoop, Errno::ELOOP),
        (StarryError::IllegalBytes, Errno::EILSEQ),
        (StarryError::InProgress, Errno::EINPROGRESS),
        (StarryError::Interrupted, Errno::EINTR),
        (StarryError::InvalidData, Errno::EINVAL),
        (StarryError::InvalidExecutable, Errno::ENOEXEC),
        (StarryError::MalformedExecutable, Errno::ENOEXEC),
        (StarryError::InvalidInput, Errno::EINVAL),
        (StarryError::Io, Errno::EIO),
        (StarryError::IsADirectory, Errno::EISDIR),
        (StarryError::NameTooLong, Errno::ENAMETOOLONG),
        (StarryError::NoMemory, Errno::ENOMEM),
        (StarryError::NoSuchDevice, Errno::ENODEV),
        (StarryError::NoSuchDeviceOrAddress, Errno::ENXIO),
        (StarryError::NoSuchProcess, Errno::ESRCH),
        (StarryError::NotADirectory, Errno::ENOTDIR),
        (StarryError::NotASocket, Errno::ENOTSOCK),
        (StarryError::NotATty, Errno::ENOTTY),
        (StarryError::NotFound, Errno::ENOENT),
        (StarryError::OperationNotPermitted, Errno::EPERM),
        (StarryError::OperationNotSupported, Errno::EOPNOTSUPP),
        (StarryError::OutOfRange, Errno::ERANGE),
        (StarryError::PermissionDenied, Errno::EACCES),
        (StarryError::ReadOnlyFilesystem, Errno::EROFS),
        (StarryError::ResourceBusy, Errno::EBUSY),
        (StarryError::StorageFull, Errno::ENOSPC),
        (StarryError::TimedOut, Errno::ETIMEDOUT),
        (StarryError::TooManyOpenFiles, Errno::EMFILE),
        (StarryError::UnexpectedEof, Errno::EIO),
        (StarryError::Unsupported, Errno::ENOSYS),
        (StarryError::WouldBlock, Errno::EAGAIN),
        (StarryError::WriteZero, Errno::EIO),
        (StarryError::Format(core::fmt::Error), Errno::EINVAL),
        (StarryError::TaskInterrupted(Interrupted), Errno::EINTR),
        (StarryError::Task(TaskError::WouldBlock), Errno::EAGAIN),
        (
            StarryError::Runtime(RuntimeError::SerialNotStarted),
            Errno::EFAULT,
        ),
        (
            StarryError::Runtime(RuntimeError::SerialControlBusy),
            Errno::EBUSY,
        ),
        (
            StarryError::Runtime(RuntimeError::SerialConfig(ConfigError::InvalidBaudrate)),
            Errno::EINVAL,
        ),
        (
            StarryError::Runtime(RuntimeError::SerialConfig(ConfigError::UnsupportedDataBits)),
            Errno::EINVAL,
        ),
        (
            StarryError::Runtime(RuntimeError::SerialConfig(ConfigError::UnsupportedStopBits)),
            Errno::EINVAL,
        ),
        (
            StarryError::Runtime(RuntimeError::SerialConfig(ConfigError::UnsupportedParity)),
            Errno::EINVAL,
        ),
        (
            StarryError::Runtime(RuntimeError::SerialConfig(ConfigError::Timeout)),
            Errno::ETIMEDOUT,
        ),
        (
            StarryError::Runtime(RuntimeError::SerialConfig(ConfigError::RegisterError)),
            Errno::EIO,
        ),
        (
            StarryError::Runtime(RuntimeError::WouldBlock),
            Errno::EAGAIN,
        ),
        (
            StarryError::Runtime(RuntimeError::OperationNotSupported),
            Errno::EOPNOTSUPP,
        ),
        (
            StarryError::Runtime(RuntimeError::InvalidCpu { cpu: 99 }),
            Errno::EINVAL,
        ),
        (
            StarryError::Dma {
                operation: DmaOperation::BufferAllocation,
                source: DmaError::NoMemory,
            },
            Errno::ENOMEM,
        ),
        (
            StarryError::Dma {
                operation: DmaOperation::DeviceIo,
                source: DmaError::NullPointer,
            },
            Errno::EINVAL,
        ),
    ])
}

#[cfg(all(test, not(axtest)))]
fn domain_errno_mappings_hold() -> bool {
    memory_errno_mappings_hold()
        && vfs_errno_mappings_hold()
        && io_errno_mappings_hold()
        && block_errno_mappings_hold()
        && leaf_errno_mappings_hold()
        && StarryError::from(Errno::EOWNERDEAD).linux_errno() == Errno::EOWNERDEAD
        && StarryError::from(Errno::new(4094)).linux_errno().into_raw() == 4094
}

/// A result returned by Starry-owned kernel operations.
pub type StarryResult<T = ()> = Result<T, StarryError>;

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn domain_errors_map_to_stable_linux_errno() {
        assert!(domain_errno_mappings_hold());
    }

    #[test]
    fn native_linux_errno_is_not_canonicalized() {
        let error = StarryError::from(Errno::EOWNERDEAD);
        assert_eq!(error.linux_errno(), Errno::EOWNERDEAD);
    }

    #[test]
    fn unknown_linux_errno_is_preserved() {
        let error = StarryError::from(Errno::new(4094));
        assert_eq!(error.linux_errno().into_raw(), 4094);
    }

    #[cfg(feature = "sg2002")]
    #[test]
    fn sg2002_tpu_errors_keep_their_linux_errno() {
        let cases = [
            (TpuError::Timeout, Errno::ETIMEDOUT),
            (TpuError::InvalidDmabuf, Errno::EINVAL),
            (TpuError::TdmaError(1), Errno::EIO),
            (TpuError::TiuError(1), Errno::EIO),
            (TpuError::NotInitialized, Errno::ENODEV),
            (TpuError::Busy, Errno::EBUSY),
            (TpuError::Interrupted, Errno::EINTR),
            (TpuError::PmuBufferNotAligned, Errno::EINVAL),
            (TpuError::DmabufNotAligned, Errno::EINVAL),
        ];
        for (error, expected) in cases {
            assert_eq!(StarryError::from(error).linux_errno(), expected);
        }
    }
}
