/// Errors owned by the `ax-io` I/O capability domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IoError {
    /// The requested address is already bound by another endpoint.
    #[error("address is already in use")]
    AddrInUse,
    /// The endpoint is already connected.
    #[error("I/O endpoint is already connected")]
    AlreadyConnected,
    /// The endpoint does not support the requested address family.
    #[error("address family is not supported")]
    AddressFamilyUnsupported,
    /// The requested I/O object already exists.
    #[error("I/O object already exists")]
    AlreadyExists,
    /// The supplied argument list exceeds the supported limit.
    #[error("argument list is too long")]
    ArgumentListTooLong,
    /// An address supplied to the I/O operation cannot be accessed.
    #[error("bad I/O address")]
    BadAddress,
    /// The supplied file descriptor is invalid.
    #[error("bad file descriptor")]
    BadFileDescriptor,
    /// The I/O object cannot perform the operation in its current state.
    #[error("I/O object is in an invalid state")]
    BadState,
    /// The peer has closed a pipe used by the operation.
    #[error("broken pipe")]
    BrokenPipe,
    /// The remote endpoint refused the connection.
    #[error("connection refused")]
    ConnectionRefused,
    /// The peer reset an established connection.
    #[error("connection reset")]
    ConnectionReset,
    /// The operation would cross filesystem or device boundaries.
    #[error("operation crosses devices")]
    CrossesDevices,
    /// The directory must be empty before the operation can complete.
    #[error("directory is not empty")]
    DirectoryNotEmpty,
    /// The operation requires a destination address.
    #[error("destination address is required")]
    DestAddrRequired,
    /// Path traversal encountered a filesystem loop.
    #[error("filesystem traversal loop detected")]
    FilesystemLoop,
    /// The requested file size exceeds the supported limit.
    #[error("file is too large")]
    FileTooLarge,
    /// Input contains an invalid byte sequence.
    #[error("illegal byte sequence")]
    IllegalBytes,
    /// The nonblocking I/O operation has started but is not complete.
    #[error("I/O operation is in progress")]
    InProgress,
    /// The operation was interrupted before completion.
    #[error("I/O operation interrupted")]
    Interrupted,
    /// Input data is malformed for the requested operation.
    #[error("invalid I/O data")]
    InvalidData,
    /// The executable image has an unsupported or malformed format.
    #[error("invalid executable format")]
    InvalidExecutable,
    /// An input parameter is invalid for the requested operation.
    #[error("invalid I/O input")]
    InvalidInput,
    /// The underlying I/O operation failed without a more specific reason.
    #[error("I/O operation failed")]
    Io,
    /// The operation expected a non-directory object.
    #[error("I/O object is a directory")]
    IsADirectory,
    /// A path component or object name exceeds the supported limit.
    #[error("name is too long")]
    NameTooLong,
    /// The message exceeds the endpoint's supported size.
    #[error("message is too long")]
    MessageTooLong,
    /// Memory required by the operation could not be allocated.
    #[error("I/O allocation failed")]
    NoMemory,
    /// The requested device does not exist.
    #[error("device does not exist")]
    NoSuchDevice,
    /// The requested device or address does not exist.
    #[error("device or address does not exist")]
    NoSuchDeviceOrAddress,
    /// The requested process does not exist.
    #[error("process does not exist")]
    NoSuchProcess,
    /// The operation expected a directory object.
    #[error("I/O object is not a directory")]
    NotADirectory,
    /// The operation expected a socket object.
    #[error("I/O object is not a socket")]
    NotASocket,
    /// The operation expected a terminal device.
    #[error("I/O object is not a tty")]
    NotATty,
    /// The endpoint is not connected.
    #[error("I/O endpoint is not connected")]
    NotConnected,
    /// The requested I/O object does not exist.
    #[error("I/O object was not found")]
    NotFound,
    /// The caller is not permitted to perform the operation.
    #[error("I/O operation is not permitted")]
    OperationNotPermitted,
    /// The object does not support this operation.
    #[error("I/O operation is not supported by this object")]
    OperationNotSupported,
    /// The operation produced a value outside its representable range.
    #[error("I/O result is out of range")]
    OutOfRange,
    /// Access permissions deny the requested operation.
    #[error("I/O permission denied")]
    PermissionDenied,
    /// The endpoint does not recognize the requested protocol option.
    #[error("protocol option is not supported")]
    ProtocolOptionUnsupported,
    /// The target filesystem is read-only.
    #[error("filesystem is read-only")]
    ReadOnlyFilesystem,
    /// A resource required by the operation is busy.
    #[error("I/O resource is busy")]
    ResourceBusy,
    /// The target storage has no free space.
    #[error("I/O storage is full")]
    StorageFull,
    /// The operation did not complete before its deadline.
    #[error("I/O operation timed out")]
    TimedOut,
    /// The process has reached its open-file limit.
    #[error("too many files are open")]
    TooManyOpenFiles,
    /// The input ended before the requested data was available.
    #[error("unexpected end of input")]
    UnexpectedEof,
    /// The requested I/O capability is not implemented.
    #[error("I/O capability is unavailable")]
    Unsupported,
    /// The operation cannot complete without blocking.
    #[error("I/O operation would block")]
    WouldBlock,
    /// A write operation completed without writing any data.
    #[error("write made no progress")]
    WriteZero,
}

impl IoError {
    /// Returns the stable I/O-domain error used by retry logic.
    pub const fn canonicalize(self) -> Self {
        self
    }
}

/// A result returned by an `ax-io` capability.
pub type IoResult<T = ()> = core::result::Result<T, IoError>;

/// Standard-style name for [`IoError`].
pub type Error = IoError;
/// Standard-style name for the copyable [`IoError`] kind.
pub type ErrorKind = IoError;
/// Standard-style name for [`IoResult`].
pub type Result<T = ()> = IoResult<T>;
