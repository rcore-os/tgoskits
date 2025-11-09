#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

use core::fmt;
use strum::EnumCount;

mod linux_errno {
    include!(concat!(env!("OUT_DIR"), "/linux_errno.rs"));
}

pub use linux_errno::LinuxError;

/// The error type used by ArceOS.
///
/// Similar to [`std::io::ErrorKind`].
///
/// [`std::io::ErrorKind`]: https://doc.rust-lang.org/std/io/enum.ErrorKind.html
#[repr(i32)]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, EnumCount)]
pub enum AxError {
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

/// A specialized [`Result`] type with [`AxError`] as the error type.
pub type AxResult<T = ()> = Result<T, AxError>;

/// A specialized [`Result`] type with [`LinuxError`] as the error type.
pub type LinuxResult<T = ()> = Result<T, LinuxError>;

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
        use $crate::AxError::*;
        $crate::__priv::warn!("[AxError::{:?}]", $err);
        $err
    }};
    ($err: ident, $msg: expr) => {{
        use $crate::AxError::*;
        $crate::__priv::warn!("[AxError::{:?}] {}", $err, $msg);
        $err
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

impl AxError {
    /// Returns the error description.
    pub fn as_str(&self) -> &'static str {
        use AxError::*;
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

impl TryFrom<i32> for AxError {
    type Error = i32;

    #[inline]
    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if value > 0 && value <= AxError::COUNT as i32 {
            Ok(unsafe { core::mem::transmute::<i32, AxError>(value) })
        } else {
            Err(value)
        }
    }
}

impl fmt::Display for AxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<AxError> for LinuxError {
    fn from(e: AxError) -> Self {
        use AxError::*;
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

impl TryFrom<LinuxError> for AxError {
    type Error = LinuxError;

    fn try_from(e: LinuxError) -> Result<Self, Self::Error> {
        use AxError::*;
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

    use crate::{AxError, LinuxError};

    #[test]
    fn test_try_from() {
        let max_code = AxError::COUNT as i32;
        assert_eq!(max_code, 43);
        assert_eq!(max_code, AxError::WriteZero.code());

        assert_eq!(AxError::AddrInUse.code(), 1);
        assert_eq!(Ok(AxError::AddrInUse), AxError::try_from(1));
        assert_eq!(Ok(AxError::AlreadyConnected), AxError::try_from(2));
        assert_eq!(Ok(AxError::WriteZero), AxError::try_from(max_code));
        assert_eq!(Err(max_code + 1), AxError::try_from(max_code + 1));
        assert_eq!(Err(0), AxError::try_from(0));
        assert_eq!(Err(-1), AxError::try_from(-1));
        assert_eq!(Err(i32::MAX), AxError::try_from(i32::MAX));
    }

    #[test]
    fn test_conversion() {
        for i in 1.. {
            let Ok(err) = LinuxError::try_from(i) else {
                break;
            };
            assert_eq!(err as i32, i);
            if let Ok(ax_err) = AxError::try_from(err) {
                assert_eq!(LinuxError::from(ax_err), err);
            }
        }
    }
}
