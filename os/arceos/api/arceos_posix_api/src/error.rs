#[cfg(feature = "fs")]
use ax_fs_ng::VfsError;
use ax_io::IoError;
#[cfg(feature = "net")]
use ax_net::NetError;
use syscalls::Errno;

/// Errors owned by the ArceOS POSIX compatibility layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PosixError {
    /// An exact Linux ABI errno selected by POSIX validation logic.
    #[error("Linux ABI error {0}")]
    Errno(Errno),
    /// A filesystem operation failed before reaching the ABI boundary.
    #[cfg(feature = "fs")]
    #[error(transparent)]
    Vfs(#[from] VfsError),
    /// A network operation failed before reaching the ABI boundary.
    #[cfg(feature = "net")]
    #[error(transparent)]
    Net(#[from] NetError),
    /// An I/O capability operation failed before reaching the ABI boundary.
    #[error(transparent)]
    Io(#[from] IoError),
}

impl PosixError {
    pub const EAGAIN: Self = Self::Errno(Errno::EAGAIN);
    pub const EBADF: Self = Self::Errno(Errno::EBADF);
    pub const EBUSY: Self = Self::Errno(Errno::EBUSY);
    pub const EDEADLK: Self = Self::Errno(Errno::EDEADLK);
    pub const EEXIST: Self = Self::Errno(Errno::EEXIST);
    pub const EFAULT: Self = Self::Errno(Errno::EFAULT);
    pub const EINTR: Self = Self::Errno(Errno::EINTR);
    pub const EINVAL: Self = Self::Errno(Errno::EINVAL);
    pub const EISCONN: Self = Self::Errno(Errno::EISCONN);
    pub const EISDIR: Self = Self::Errno(Errno::EISDIR);
    pub const ELOOP: Self = Self::Errno(Errno::ELOOP);
    pub const EMFILE: Self = Self::Errno(Errno::EMFILE);
    pub const ENOENT: Self = Self::Errno(Errno::ENOENT);
    pub const ENOPROTOOPT: Self = Self::Errno(Errno::ENOPROTOOPT);
    pub const ENOTDIR: Self = Self::Errno(Errno::ENOTDIR);
    pub const EOPNOTSUPP: Self = Self::Errno(Errno::EOPNOTSUPP);
    pub const EPERM: Self = Self::Errno(Errno::EPERM);
    pub const ERANGE: Self = Self::Errno(Errno::ERANGE);

    /// Returns the Linux errno exposed at the C ABI boundary.
    pub fn errno(self) -> Errno {
        match self {
            Self::Errno(errno) => errno,
            #[cfg(feature = "fs")]
            Self::Vfs(error) => vfs_error_to_errno(error),
            #[cfg(feature = "net")]
            Self::Net(error) => io_error_to_errno(error.into()),
            Self::Io(error) => io_error_to_errno(error),
        }
    }
}

impl From<Errno> for PosixError {
    fn from(errno: Errno) -> Self {
        Self::Errno(errno)
    }
}

/// A result returned by POSIX compatibility logic before C ABI conversion.
pub type PosixResult<T = ()> = Result<T, PosixError>;

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

#[cfg(feature = "fs")]
fn vfs_error_to_errno(error: VfsError) -> Errno {
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
