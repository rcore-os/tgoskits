#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

use core::fmt;
use strum::EnumCount;

mod linux_errno {
    include!(concat!(env!("OUT_DIR"), "/linux_errno.rs"));
}

pub use linux_errno::LinuxError;

/// The error kind type used by ArceOS.
///
/// Similar to [`std::io::ErrorKind`].
///
/// [`std::io::ErrorKind`]: https://doc.rust-lang.org/std/io/enum.ErrorKind.html
#[repr(i32)]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, EnumCount)]
pub enum AxErrorKind {
    /// A socket address could not be bound because the address is already in use elsewhere.
    AddrInUse = 1,
    /// The socket is already connected.
    AlreadyConnected,
    /// An entity already exists, often a file.
    AlreadyExists,
    /// Program argument list too long.
    ArgumentListTooLong,
    /// Bad address.
    BadAddress,
    /// Bad file descriptor.
    BadFileDescriptor,
    /// Bad internal state.
    BadState,
    /// Broken pipe
    BrokenPipe,
    /// The connection was refused by the remote server.
    ConnectionRefused,
    /// The connection was reset by the remote server.
    ConnectionReset,
    /// Cross-device or cross-filesystem (hard) link or rename.
    CrossesDevices,
    /// A non-empty directory was specified where an empty directory was expected.
    DirectoryNotEmpty,
    /// Loop in the filesystem or IO subsystem; often, too many levels of
    /// symbolic links.
    FilesystemLoop,
    /// Illegal byte sequence.
    IllegalBytes,
    /// The operation was partially successful and needs to be checked later on
    /// due to not blocking.
    InProgress,
    /// This operation was interrupted.
    Interrupted,
    /// Data not valid for the operation were encountered.
    ///
    /// Unlike [`InvalidInput`], this typically means that the operation
    /// parameters were valid, however the error was caused by malformed
    /// input data.
    ///
    /// For example, a function that reads a file into a string will error with
    /// `InvalidData` if the file's contents are not valid UTF-8.
    ///
    /// [`InvalidInput`]: AxError::InvalidInput
    InvalidData,
    /// Invalid executable format.
    InvalidExecutable,
    /// Invalid parameter/argument.
    InvalidInput,
    /// Input/output error.
    Io,
    /// The filesystem object is, unexpectedly, a directory.
    IsADirectory,
    /// Filename is too long.
    NameTooLong,
    /// Not enough space/cannot allocate memory.
    NoMemory,
    /// No such device.
    NoSuchDevice,
    /// No such process.
    NoSuchProcess,
    /// A filesystem object is, unexpectedly, not a directory.
    NotADirectory,
    /// The specified entity is not a socket.
    NotASocket,
    /// Not a typewriter.
    NotATty,
    /// The network operation failed because it was not connected yet.
    NotConnected,
    /// The requested entity is not found.
    NotFound,
    /// Operation not permitted.
    OperationNotPermitted,
    /// Operation not supported.
    OperationNotSupported,
    /// Result out of range.
    OutOfRange,
    /// The operation lacked the necessary privileges to complete.
    PermissionDenied,
    /// The filesystem or storage medium is read-only, but a write operation was attempted.
    ReadOnlyFilesystem,
    /// Device or resource is busy.
    ResourceBusy,
    /// The underlying storage (typically, a filesystem) is full.
    StorageFull,
    /// The I/O operation’s timeout expired, causing it to be canceled.
    TimedOut,
    /// The process has too many files open.
    TooManyOpenFiles,
    /// An error returned when an operation could not be completed because an
    /// "end of file" was reached prematurely.
    UnexpectedEof,
    /// This operation is unsupported or unimplemented.
    Unsupported,
    /// The operation needs to block to complete, but the blocking operation was
    /// requested to not occur.
    WouldBlock,
    /// An error returned when an operation could not be completed because a
    /// call to `write()` returned [`Ok(0)`](Ok).
    WriteZero,
}

impl AxErrorKind {
    /// Returns the error description.
    pub fn as_str(&self) -> &'static str {
        use AxErrorKind::*;
        match *self {
            AddrInUse => "Address in use",
            AlreadyConnected => "Already connected",
            AlreadyExists => "Entity already exists",
            ArgumentListTooLong => "Argument list too long",
            BadAddress => "Bad address",
            BadFileDescriptor => "Bad file descriptor",
            BadState => "Bad internal state",
            BrokenPipe => "Broken pipe",
            ConnectionRefused => "Connection refused",
            ConnectionReset => "Connection reset",
            CrossesDevices => "Cross-device link or rename",
            DirectoryNotEmpty => "Directory not empty",
            FilesystemLoop => "Filesystem loop or indirection limit",
            IllegalBytes => "Illegal byte sequence",
            InProgress => "Operation in progress",
            Interrupted => "Operation interrupted",
            InvalidData => "Invalid data",
            InvalidExecutable => "Invalid executable format",
            InvalidInput => "Invalid input parameter",
            Io => "I/O error",
            IsADirectory => "Is a directory",
            NameTooLong => "Filename too long",
            NoMemory => "Out of memory",
            NoSuchDevice => "No such device",
            NoSuchProcess => "No such process",
            NotADirectory => "Not a directory",
            NotASocket => "Not a socket",
            NotATty => "Inappropriate ioctl for device",
            NotConnected => "Not connected",
            NotFound => "Entity not found",
            OperationNotPermitted => "Operation not permitted",
            OperationNotSupported => "Operation not supported",
            OutOfRange => "Result out of range",
            PermissionDenied => "Permission denied",
            ReadOnlyFilesystem => "Read-only filesystem",
            ResourceBusy => "Resource busy",
            StorageFull => "No storage space",
            TimedOut => "Timed out",
            TooManyOpenFiles => "Too many open files",
            UnexpectedEof => "Unexpected end of file",
            Unsupported => "Operation not supported",
            WouldBlock => "Operation would block",
            WriteZero => "Write zero",
        }
    }

    /// Returns the error code value in `i32`.
    pub const fn code(self) -> i32 {
        self as i32
    }
}

impl TryFrom<i32> for AxErrorKind {
    type Error = i32;

    #[inline]
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if value > 0 && value <= AxErrorKind::COUNT as i32 {
            Ok(unsafe { core::mem::transmute::<i32, AxErrorKind>(value) })
        } else {
            Err(value)
        }
    }
}

impl fmt::Display for AxErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<AxErrorKind> for LinuxError {
    fn from(e: AxErrorKind) -> Self {
        use AxErrorKind::*;
        use LinuxError::*;
        match e {
            AddrInUse => EADDRINUSE,
            AlreadyConnected => EISCONN,
            AlreadyExists => EEXIST,
            ArgumentListTooLong => E2BIG,
            BadAddress | BadState => EFAULT,
            BadFileDescriptor => EBADF,
            BrokenPipe => EPIPE,
            ConnectionRefused => ECONNREFUSED,
            ConnectionReset => ECONNRESET,
            CrossesDevices => EXDEV,
            DirectoryNotEmpty => ENOTEMPTY,
            FilesystemLoop => ELOOP,
            IllegalBytes => EILSEQ,
            InProgress => EINPROGRESS,
            Interrupted => EINTR,
            InvalidExecutable => ENOEXEC,
            InvalidInput | InvalidData => EINVAL,
            Io => EIO,
            IsADirectory => EISDIR,
            NameTooLong => ENAMETOOLONG,
            NoMemory => ENOMEM,
            NoSuchDevice => ENODEV,
            NoSuchProcess => ESRCH,
            NotADirectory => ENOTDIR,
            NotASocket => ENOTSOCK,
            NotATty => ENOTTY,
            NotConnected => ENOTCONN,
            NotFound => ENOENT,
            OperationNotPermitted => EPERM,
            OperationNotSupported => EOPNOTSUPP,
            OutOfRange => ERANGE,
            PermissionDenied => EACCES,
            ReadOnlyFilesystem => EROFS,
            ResourceBusy => EBUSY,
            StorageFull => ENOSPC,
            TimedOut => ETIMEDOUT,
            TooManyOpenFiles => EMFILE,
            UnexpectedEof | WriteZero => EIO,
            Unsupported => ENOSYS,
            WouldBlock => EAGAIN,
        }
    }
}

impl TryFrom<LinuxError> for AxErrorKind {
    type Error = LinuxError;

    fn try_from(e: LinuxError) -> Result<Self, Self::Error> {
        use AxErrorKind::*;
        use LinuxError::*;
        Ok(match e {
            EADDRINUSE => AddrInUse,
            EISCONN => AlreadyConnected,
            EEXIST => AlreadyExists,
            E2BIG => ArgumentListTooLong,
            EFAULT => BadAddress,
            EBADF => BadFileDescriptor,
            EPIPE => BrokenPipe,
            ECONNREFUSED => ConnectionRefused,
            ECONNRESET => ConnectionReset,
            EXDEV => CrossesDevices,
            ENOTEMPTY => DirectoryNotEmpty,
            ELOOP => FilesystemLoop,
            EILSEQ => IllegalBytes,
            EINPROGRESS => InProgress,
            EINTR => Interrupted,
            ENOEXEC => InvalidExecutable,
            EINVAL => InvalidInput,
            EIO => Io,
            EISDIR => IsADirectory,
            ENAMETOOLONG => NameTooLong,
            ENOMEM => NoMemory,
            ENODEV => NoSuchDevice,
            ESRCH => NoSuchProcess,
            ENOTDIR => NotADirectory,
            ENOTSOCK => NotASocket,
            ENOTTY => NotATty,
            ENOTCONN => NotConnected,
            ENOENT => NotFound,
            EPERM => OperationNotPermitted,
            EOPNOTSUPP => OperationNotSupported,
            ERANGE => OutOfRange,
            EACCES => PermissionDenied,
            EROFS => ReadOnlyFilesystem,
            EBUSY => ResourceBusy,
            ENOSPC => StorageFull,
            ETIMEDOUT => TimedOut,
            EMFILE => TooManyOpenFiles,
            ENOSYS => Unsupported,
            EAGAIN => WouldBlock,
            _ => {
                return Err(e);
            }
        })
    }
}

/// The error type used by ArceOS.
#[repr(transparent)]
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AxError(i32);

enum AxErrorData {
    Ax(AxErrorKind),
    Linux(LinuxError),
}

impl AxError {
    const fn new(data: AxErrorData) -> Self {
        match data {
            AxErrorData::Ax(kind) => Self::new_ax(kind),
            AxErrorData::Linux(kind) => Self::new_linux(kind),
        }
    }

    const fn new_ax(kind: AxErrorKind) -> Self {
        AxError(kind.code())
    }

    const fn new_linux(kind: LinuxError) -> Self {
        AxError(-kind.code())
    }

    const fn data(&self) -> AxErrorData {
        if self.0 < 0 {
            AxErrorData::Linux(unsafe { core::mem::transmute::<i32, LinuxError>(-self.0) })
        } else {
            AxErrorData::Ax(unsafe { core::mem::transmute::<i32, AxErrorKind>(self.0) })
        }
    }

    /// Returns the error code value in `i32`.
    pub const fn code(self) -> i32 {
        self.0
    }

    /// Returns a canonicalized version of this error.
    ///
    /// This method tries to convert [`LinuxError`] variants into their
    /// corresponding [`AxErrorKind`] variants if possible.
    pub fn canonicalize(self) -> Self {
        AxErrorKind::try_from(self).map_or_else(Into::into, Into::into)
    }
}

impl From<AxErrorKind> for AxError {
    fn from(e: AxErrorKind) -> Self {
        AxError::new_ax(e)
    }
}

impl From<LinuxError> for AxError {
    fn from(e: LinuxError) -> Self {
        AxError::new(AxErrorData::Linux(e))
    }
}

impl From<AxError> for LinuxError {
    fn from(e: AxError) -> Self {
        match e.data() {
            AxErrorData::Ax(kind) => LinuxError::from(kind),
            AxErrorData::Linux(kind) => kind,
        }
    }
}

impl TryFrom<AxError> for AxErrorKind {
    type Error = LinuxError;

    fn try_from(e: AxError) -> Result<Self, Self::Error> {
        match e.data() {
            AxErrorData::Ax(kind) => Ok(kind),
            AxErrorData::Linux(e) => e.try_into(),
        }
    }
}

impl TryFrom<i32> for AxError {
    type Error = i32;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if AxErrorKind::try_from(value).is_ok() || LinuxError::try_from(-value).is_ok() {
            Ok(AxError(value))
        } else {
            Err(value)
        }
    }
}

impl fmt::Debug for AxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.data() {
            AxErrorData::Ax(kind) => write!(f, "AxErrorKind::{:?}", kind),
            AxErrorData::Linux(kind) => write!(f, "LinuxError::{:?}", kind),
        }
    }
}

impl fmt::Display for AxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.data() {
            AxErrorData::Ax(kind) => write!(f, "{}", kind),
            AxErrorData::Linux(kind) => write!(f, "{}", kind),
        }
    }
}

#[allow(non_upper_case_globals)]
impl AxError {
    pub const AddrInUse: Self = Self::new_ax(AxErrorKind::AddrInUse);
    pub const AlreadyConnected: Self = Self::new_ax(AxErrorKind::AlreadyConnected);
    pub const AlreadyExists: Self = Self::new_ax(AxErrorKind::AlreadyExists);
    pub const ArgumentListTooLong: Self = Self::new_ax(AxErrorKind::ArgumentListTooLong);
    pub const BadAddress: Self = Self::new_ax(AxErrorKind::BadAddress);
    pub const BadFileDescriptor: Self = Self::new_ax(AxErrorKind::BadFileDescriptor);
    pub const BadState: Self = Self::new_ax(AxErrorKind::BadState);
    pub const BrokenPipe: Self = Self::new_ax(AxErrorKind::BrokenPipe);
    pub const ConnectionRefused: Self = Self::new_ax(AxErrorKind::ConnectionRefused);
    pub const ConnectionReset: Self = Self::new_ax(AxErrorKind::ConnectionReset);
    pub const CrossesDevices: Self = Self::new_ax(AxErrorKind::CrossesDevices);
    pub const DirectoryNotEmpty: Self = Self::new_ax(AxErrorKind::DirectoryNotEmpty);
    pub const FilesystemLoop: Self = Self::new_ax(AxErrorKind::FilesystemLoop);
    pub const IllegalBytes: Self = Self::new_ax(AxErrorKind::IllegalBytes);
    pub const InProgress: Self = Self::new_ax(AxErrorKind::InProgress);
    pub const Interrupted: Self = Self::new_ax(AxErrorKind::Interrupted);
    pub const InvalidData: Self = Self::new_ax(AxErrorKind::InvalidData);
    pub const InvalidExecutable: Self = Self::new_ax(AxErrorKind::InvalidExecutable);
    pub const InvalidInput: Self = Self::new_ax(AxErrorKind::InvalidInput);
    pub const Io: Self = Self::new_ax(AxErrorKind::Io);
    pub const IsADirectory: Self = Self::new_ax(AxErrorKind::IsADirectory);
    pub const NameTooLong: Self = Self::new_ax(AxErrorKind::NameTooLong);
    pub const NoMemory: Self = Self::new_ax(AxErrorKind::NoMemory);
    pub const NoSuchDevice: Self = Self::new_ax(AxErrorKind::NoSuchDevice);
    pub const NoSuchProcess: Self = Self::new_ax(AxErrorKind::NoSuchProcess);
    pub const NotADirectory: Self = Self::new_ax(AxErrorKind::NotADirectory);
    pub const NotASocket: Self = Self::new_ax(AxErrorKind::NotASocket);
    pub const NotATty: Self = Self::new_ax(AxErrorKind::NotATty);
    pub const NotConnected: Self = Self::new_ax(AxErrorKind::NotConnected);
    pub const NotFound: Self = Self::new_ax(AxErrorKind::NotFound);
    pub const OperationNotPermitted: Self = Self::new_ax(AxErrorKind::OperationNotPermitted);
    pub const OperationNotSupported: Self = Self::new_ax(AxErrorKind::OperationNotSupported);
    pub const OutOfRange: Self = Self::new_ax(AxErrorKind::OutOfRange);
    pub const PermissionDenied: Self = Self::new_ax(AxErrorKind::PermissionDenied);
    pub const ReadOnlyFilesystem: Self = Self::new_ax(AxErrorKind::ReadOnlyFilesystem);
    pub const ResourceBusy: Self = Self::new_ax(AxErrorKind::ResourceBusy);
    pub const StorageFull: Self = Self::new_ax(AxErrorKind::StorageFull);
    pub const TimedOut: Self = Self::new_ax(AxErrorKind::TimedOut);
    pub const TooManyOpenFiles: Self = Self::new_ax(AxErrorKind::TooManyOpenFiles);
    pub const UnexpectedEof: Self = Self::new_ax(AxErrorKind::UnexpectedEof);
    pub const Unsupported: Self = Self::new_ax(AxErrorKind::Unsupported);
    pub const WouldBlock: Self = Self::new_ax(AxErrorKind::WouldBlock);
    pub const WriteZero: Self = Self::new_ax(AxErrorKind::WriteZero);
}

/// A specialized [`Result`] type with [`AxError`] as the error type.
pub type AxResult<T = ()> = Result<T, AxError>;

/// Convenience method to construct an [`AxError`] type while printing a warning
/// message.
///
/// # Examples
///
/// ```
/// # use axerrno::{ax_err_type, AxError};
/// #
/// // Also print "[AxError::AlreadyExists]" if the `log` crate is enabled.
/// assert_eq!(
///     ax_err_type!(AlreadyExists),
///     AxError::AlreadyExists,
/// );
///
/// // Also print "[AxError::BadAddress] the address is 0!" if the `log` crate
/// // is enabled.
/// assert_eq!(
///     ax_err_type!(BadAddress, "the address is 0!"),
///     AxError::BadAddress,
/// );
/// ```
#[macro_export]
macro_rules! ax_err_type {
    ($err: ident) => {{
        use $crate::AxErrorKind::*;
        let err = $crate::AxError::from($err);
        $crate::__priv::warn!("[{:?}]", err);
        err
    }};
    ($err: ident, $msg: expr) => {{
        use $crate::AxErrorKind::*;
        let err = $crate::AxError::from($err);
        $crate::__priv::warn!("[{:?}] {}", err, $msg);
        err
    }};
}

/// Ensure a condition is true. If it is not, return from the function
/// with an error.
///
/// ## Examples
///
/// ```rust
/// # use axerrno::{ensure, ax_err, AxError, AxResult};
///
/// fn example(user_id: i32) -> AxResult {
///     ensure!(user_id > 0, ax_err!(InvalidInput));
///     // After this point, we know that `user_id` is positive.
///     let user_id = user_id as u32;
///     Ok(())
/// }
/// ```
#[macro_export]
macro_rules! ensure {
    ($predicate:expr, $context_selector:expr $(,)?) => {
        if !$predicate {
            return $context_selector;
        }
    };
}

/// Convenience method to construct an [`Err(AxError)`] type while printing a
/// warning message.
///
/// # Examples
///
/// ```
/// # use axerrno::{ax_err, AxResult, AxError};
/// #
/// // Also print "[AxError::AlreadyExists]" if the `log` crate is enabled.
/// assert_eq!(
///     ax_err!(AlreadyExists),
///     AxResult::<()>::Err(AxError::AlreadyExists),
/// );
///
/// // Also print "[AxError::BadAddress] the address is 0!" if the `log` crate is enabled.
/// assert_eq!(
///     ax_err!(BadAddress, "the address is 0!"),
///     AxResult::<()>::Err(AxError::BadAddress),
/// );
/// ```
/// [`Err(AxError)`]: Err
#[macro_export]
macro_rules! ax_err {
    ($err: ident) => {
        Err($crate::ax_err_type!($err))
    };
    ($err: ident, $msg: expr) => {
        Err($crate::ax_err_type!($err, $msg))
    };
}

/// Throws an error of type [`AxError`] with the given error code, optionally
/// with a message.
#[macro_export]
macro_rules! ax_bail {
    ($($t:tt)*) => {
        return $crate::ax_err!($($t)*);
    };
}

/// A specialized [`Result`] type with [`LinuxError`] as the error type.
pub type LinuxResult<T = ()> = Result<T, LinuxError>;

impl fmt::Display for LinuxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[doc(hidden)]
pub mod __priv {
    pub use log::warn;
}

#[cfg(test)]
mod tests {
    use strum::EnumCount;

    use crate::{AxError, AxErrorKind, LinuxError};

    #[test]
    fn test_try_from() {
        let max_code = AxErrorKind::COUNT as i32;
        assert_eq!(max_code, 43);
        assert_eq!(max_code, AxError::WriteZero.code());

        assert_eq!(AxError::AddrInUse.code(), 1);
        assert_eq!(Ok(AxError::AddrInUse), AxError::try_from(1));
        assert_eq!(Ok(AxError::AlreadyConnected), AxError::try_from(2));
        assert_eq!(Ok(AxError::WriteZero), AxError::try_from(max_code));
        assert_eq!(Err(max_code + 1), AxError::try_from(max_code + 1));
        assert_eq!(Err(0), AxError::try_from(0));
        assert_eq!(Err(i32::MAX), AxError::try_from(i32::MAX));
    }

    #[test]
    fn test_conversion() {
        for i in 1.. {
            let Ok(err) = LinuxError::try_from(i) else {
                break;
            };
            assert_eq!(err as i32, i);
            let e = AxError::from(err);
            assert_eq!(e.code(), -i);
            assert_eq!(LinuxError::from(e), err);
        }
    }
}
