use ax_io::IoError;
use ax_task::future::{Elapsed, Interrupted, PollIoError};

/// Errors owned by the network and socket domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NetError {
    /// The requested local address is already bound.
    #[error("network address is already in use")]
    AddrInUse,
    /// The socket already has an established peer.
    #[error("socket is already connected")]
    AlreadyConnected,
    /// The requested network object already exists.
    #[error("network object already exists")]
    AlreadyExists,
    /// The network object cannot perform the operation in its current state.
    #[error("network object is in an invalid state")]
    BadState,
    /// A user or filesystem address used by the socket cannot be accessed.
    #[error("bad network address")]
    BadAddress,
    /// A descriptor used by the network operation is invalid.
    #[error("bad network file descriptor")]
    BadFileDescriptor,
    /// The peer has closed the write side of a stream.
    #[error("network stream is broken")]
    BrokenPipe,
    /// The destination rejected the connection attempt.
    #[error("connection was refused")]
    ConnectionRefused,
    /// The peer reset an established connection.
    #[error("connection was reset")]
    ConnectionReset,
    /// A path-backed socket operation would cross filesystem devices.
    #[error("socket path operation crosses devices")]
    CrossesDevices,
    /// A directory involved in a socket path operation is not empty.
    #[error("socket path directory is not empty")]
    DirectoryNotEmpty,
    /// A datagram operation requires a destination address.
    #[error("destination address is required")]
    DestAddrRequired,
    /// Socket-path traversal encountered a filesystem loop.
    #[error("socket path traversal loop detected")]
    FilesystemLoop,
    /// A path-backed socket object exceeds the supported filesystem limit.
    #[error("socket path object is too large")]
    FileTooLarge,
    /// A nonblocking connection attempt is still in progress.
    #[error("connection is in progress")]
    InProgress,
    /// A socket address, option, or operation argument is invalid.
    #[error("invalid network input")]
    InvalidInput,
    /// A path-backed socket object contains invalid filesystem data.
    #[error("socket path data is invalid")]
    InvalidData,
    /// A socket path unexpectedly refers to a directory.
    #[error("socket path is a directory")]
    IsADirectory,
    /// The packet or message exceeds the transport limit.
    #[error("network message is too long")]
    MessageTooLong,
    /// A socket path component exceeds the filesystem name limit.
    #[error("socket path name is too long")]
    NameTooLong,
    /// Memory required by the network operation could not be allocated.
    #[error("network allocation failed")]
    NoMemory,
    /// The requested interface does not exist.
    #[error("network device does not exist")]
    NoSuchDevice,
    /// No interface owns the address, or no route reaches the destination.
    #[error("network device or address does not exist")]
    NoSuchDeviceOrAddress,
    /// The socket has no connected peer.
    #[error("socket is not connected")]
    NotConnected,
    /// The requested network object was not found.
    #[error("network object was not found")]
    NotFound,
    /// A socket path component expected to be a directory is not one.
    #[error("socket path component is not a directory")]
    NotADirectory,
    /// The selected object is not a socket.
    #[error("object is not a socket")]
    NotASocket,
    /// The selected socket object is not a terminal.
    #[error("socket object is not a tty")]
    NotATty,
    /// The caller is not permitted to change the requested network state.
    #[error("network operation is not permitted")]
    OperationNotPermitted,
    /// The socket or transport does not implement the requested operation.
    #[error("network operation is not supported")]
    OperationNotSupported,
    /// The address belongs to a family unsupported by this socket.
    #[error("socket address family is not supported")]
    AddressFamilyUnsupported,
    /// The socket does not recognize the requested protocol option.
    #[error("socket protocol option is not supported")]
    ProtocolOptionUnsupported,
    /// Filesystem permissions deny access to a path-backed socket.
    #[error("socket path permission denied")]
    PermissionDenied,
    /// A path-backed socket operation attempted to modify a read-only filesystem.
    #[error("socket path filesystem is read-only")]
    ReadOnlyFilesystem,
    /// A queue or network resource is currently busy.
    #[error("network resource is busy")]
    ResourceBusy,
    /// Storage used by a path-backed socket has no free space.
    #[error("socket path storage is full")]
    StorageFull,
    /// The network operation did not complete before its deadline.
    #[error("network operation timed out")]
    TimedOut,
    /// The requested network capability is unavailable in this build.
    #[error("network capability is unavailable")]
    Unsupported,
    /// A secured wireless connection cannot obtain trusted runtime entropy.
    #[error("trusted wireless connection entropy is unavailable")]
    EntropyUnavailable,
    /// The operation cannot complete without blocking.
    #[error("network operation would block")]
    WouldBlock,
    /// A caller-provided I/O buffer failed while transferring packet data.
    #[error(transparent)]
    Io(#[from] IoError),
    /// A network backend or path-backed namespace reported an I/O failure.
    #[error("network backend I/O failed")]
    BackendIo,
    /// A blocking network wait was interrupted by the current task.
    #[error(transparent)]
    Interrupted(#[from] Interrupted),
    /// A blocking network operation exceeded its task deadline.
    #[error(transparent)]
    Elapsed(#[from] Elapsed),
}

/// A result returned by the network and socket domain.
pub type NetResult<T = ()> = Result<T, NetError>;

impl PollIoError for NetError {
    fn is_would_block(&self) -> bool {
        matches!(self, Self::WouldBlock)
    }

    fn interrupted(error: Interrupted) -> Self {
        error.into()
    }
}

impl From<NetError> for IoError {
    fn from(error: NetError) -> Self {
        match error {
            NetError::AddrInUse => Self::AddrInUse,
            NetError::AlreadyConnected => Self::AlreadyConnected,
            NetError::AlreadyExists => Self::AlreadyExists,
            NetError::BadState => Self::BadState,
            NetError::BadAddress => Self::BadAddress,
            NetError::BadFileDescriptor => Self::BadFileDescriptor,
            NetError::BrokenPipe => Self::BrokenPipe,
            NetError::ConnectionRefused => Self::ConnectionRefused,
            NetError::ConnectionReset => Self::ConnectionReset,
            NetError::CrossesDevices => Self::CrossesDevices,
            NetError::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            NetError::DestAddrRequired => Self::DestAddrRequired,
            NetError::FilesystemLoop => Self::FilesystemLoop,
            NetError::FileTooLarge => Self::FileTooLarge,
            NetError::InProgress => Self::InProgress,
            NetError::InvalidInput => Self::InvalidInput,
            NetError::InvalidData => Self::InvalidData,
            NetError::IsADirectory => Self::IsADirectory,
            NetError::MessageTooLong => Self::MessageTooLong,
            NetError::NameTooLong => Self::NameTooLong,
            NetError::NoMemory => Self::NoMemory,
            NetError::NoSuchDevice => Self::NoSuchDevice,
            NetError::NoSuchDeviceOrAddress => Self::NoSuchDeviceOrAddress,
            NetError::NotConnected => Self::NotConnected,
            NetError::NotFound => Self::NotFound,
            NetError::NotADirectory => Self::NotADirectory,
            NetError::NotASocket => Self::NotASocket,
            NetError::NotATty => Self::NotATty,
            NetError::OperationNotPermitted => Self::OperationNotPermitted,
            NetError::OperationNotSupported => Self::OperationNotSupported,
            NetError::AddressFamilyUnsupported => Self::AddressFamilyUnsupported,
            NetError::ProtocolOptionUnsupported => Self::ProtocolOptionUnsupported,
            NetError::PermissionDenied => Self::PermissionDenied,
            NetError::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            NetError::ResourceBusy => Self::ResourceBusy,
            NetError::StorageFull => Self::StorageFull,
            NetError::TimedOut | NetError::Elapsed(_) => Self::TimedOut,
            NetError::Unsupported => Self::Unsupported,
            NetError::EntropyUnavailable => Self::OperationNotSupported,
            NetError::WouldBlock => Self::WouldBlock,
            NetError::Io(error) => error,
            NetError::BackendIo => Self::Io,
            NetError::Interrupted(_) => Self::Interrupted,
        }
    }
}
