use ax_api::ApiError;
use ax_io::IoError;
#[cfg(all(feature = "std-compat", feature = "fs"))]
use syscalls::Errno;

/// Errors owned by the ArceOS standard-library facade.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StdError {
    /// A public ArceOS API operation failed.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// An `ax-io` operation failed while implementing a standard-library action.
    #[error(transparent)]
    Io(#[from] IoError),
    /// Address resolution yielded no usable socket address.
    #[error("address resolution yielded no socket addresses")]
    NoResolvedAddress,
    /// A socket address string has invalid syntax.
    #[error("invalid socket address")]
    InvalidSocketAddress,
    /// A socket port string is not a valid `u16` value.
    #[error("invalid socket port")]
    InvalidSocketPort,
    /// A datagram send operation has no destination address.
    #[error("no destination address was provided")]
    MissingDestinationAddress,
    /// Recursive directory creation is not implemented by this facade.
    #[error("recursive directory creation is not supported")]
    RecursiveDirectoryCreationUnsupported,
    /// A joined task exited without publishing its return value.
    #[error("thread exited without publishing its result")]
    ThreadResultUnavailable,
    /// The runtime reported no available CPU.
    #[error("the runtime reported no available CPUs")]
    NoAvailableCpu,
}

/// A result returned by an ArceOS standard-library facade operation.
pub type StdResult<T = ()> = Result<T, StdError>;

#[cfg(all(feature = "std-compat", feature = "fs"))]
pub(crate) fn api_error_to_errno(error: ApiError) -> Errno {
    io_error_to_errno(error.into())
}

#[cfg(all(feature = "std-compat", feature = "fs"))]
fn io_error_to_errno(error: IoError) -> Errno {
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
