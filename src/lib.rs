#![cfg_attr(not(test), no_std)]
#![feature(variant_count)]
#![doc = include_str!("../README.md")]

use core::fmt;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum AxError {
    /// A socket address could not be bound because the address is already in use elsewhere.
    AddrInUse = 1,
    /// An entity already exists, often a file.
    AlreadyExists,
    /// Bad address.
    BadAddress,
    /// Bad internal state.
    BadState,
    /// The connection was refused by the remote server,
    ConnectionRefused,
    /// The connection was reset by the remote server.
    ConnectionReset,
    /// A non-empty directory was specified where an empty directory was expected.
    DirectoryNotEmpty,
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
    /// Invalid parameter/argument.
    InvalidInput,
    /// Input/output error.
    Io,
    /// The filesystem object is, unexpectedly, a directory.
    IsADirectory,
    /// Not enough space/cannot allocate memory.
    NoMemory,
    /// A filesystem object is, unexpectedly, not a directory.
    NotADirectory,
    /// The network operation failed because it was not connected yet.
    NotConnected,
    /// The requested entity is not found.
    NotFound,
    /// The operation lacked the necessary privileges to complete.
    PermissionDenied,
    /// Device or resource is busy.
    ResourceBusy,
    /// The underlying storage (typically, a filesystem) is full.
    StorageFull,
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
    /// Program argument list too long.
    ArgumentListTooLong,
    /// Cross-device or cross-filesystem (hard) link or rename.
    CrossesDevices,
    /// Not a typewriter.
    NotATty,
    /// Filename is too long.
    NameTooLong,
    /// Bad file descriptor.
    BadFileDescriptor,
    /// Loop in the filesystem or IO subsystem; often, too many levels of
    /// symbolic links.
    FilesystemLoop,
    /// Operation not supported.
    OperationNotSupported,
    /// The operation was partially successful and needs to be checked later on
    /// due to not blocking.
    InProgress,
    /// The socket is already connected.
    AlreadyConnected,
    /// This operation was interrupted.
    Interrupted,
    /// The I/O operation’s timeout expired, causing it to be canceled.
    TimedOut,
    /// The specified entity is not a socket.
    NotASocket,
    /// Broken pipe
    BrokenPipe,
    /// The process has too many files open.
    TooManyOpenFiles,
    /// No such process.
    NoSuchProcess,
    /// Illegal byte sequence.
    IllegalBytes,
    /// Operation not permitted.
    OperationNotPermitted,
    /// Result out of range.
    OutOfRange,
    /// Invalid executable format.
    InvalidExecutable,
    /// No such device.
    NoSuchDevice,
    /// The filesystem or storage medium is read-only, but a write operation was attempted.
    ReadOnlyFilesystem,
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
            BadAddress => "Bad address",
            BadState => "Bad internal state",
            AlreadyExists => "Entity already exists",
            ConnectionRefused => "Connection refused",
            ConnectionReset => "Connection reset",
            DirectoryNotEmpty => "Directory not empty",
            InvalidData => "Invalid data",
            InvalidInput => "Invalid input parameter",
            Io => "I/O error",
            IsADirectory => "Is a directory",
            NoMemory => "Out of memory",
            NotADirectory => "Not a directory",
            NotConnected => "Not connected",
            NotFound => "Entity not found",
            PermissionDenied => "Permission denied",
            ResourceBusy => "Resource busy",
            StorageFull => "No storage space",
            UnexpectedEof => "Unexpected end of file",
            Unsupported => "Operation not supported",
            WouldBlock => "Operation would block",
            WriteZero => "Write zero",
            ArgumentListTooLong => "Argument list too long",
            CrossesDevices => "Cross-device link or rename",
            NotATty => "Inappropriate ioctl for device",
            NameTooLong => "Filename too long",
            BadFileDescriptor => "Bad file descriptor",
            FilesystemLoop => "Filesystem loop or indirection limit",
            OperationNotSupported => "Operation not supported",
            InProgress => "Operation in progress",
            AlreadyConnected => "Already connected",
            Interrupted => "Operation interrupted",
            TimedOut => "Timed out",
            NotASocket => "Not a socket",
            BrokenPipe => "Broken pipe",
            TooManyOpenFiles => "Too many open files",
            NoSuchProcess => "No such process",
            IllegalBytes => "Illegal byte sequence",
            OperationNotPermitted => "Operation not permitted",
            OutOfRange => "Result out of range",
            InvalidExecutable => "Invalid executable format",
            NoSuchDevice => "No such device",
            ReadOnlyFilesystem => "Read-only filesystem",
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
        if value > 0 && value <= core::mem::variant_count::<AxError>() as i32 {
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
        match e {
            AddrInUse => LinuxError::EADDRINUSE,
            AlreadyExists => LinuxError::EEXIST,
            BadAddress | BadState => LinuxError::EFAULT,
            ConnectionRefused => LinuxError::ECONNREFUSED,
            ConnectionReset => LinuxError::ECONNRESET,
            DirectoryNotEmpty => LinuxError::ENOTEMPTY,
            InvalidInput | InvalidData => LinuxError::EINVAL,
            Io => LinuxError::EIO,
            IsADirectory => LinuxError::EISDIR,
            NoMemory => LinuxError::ENOMEM,
            NotADirectory => LinuxError::ENOTDIR,
            NotConnected => LinuxError::ENOTCONN,
            NotFound => LinuxError::ENOENT,
            PermissionDenied => LinuxError::EACCES,
            ResourceBusy => LinuxError::EBUSY,
            StorageFull => LinuxError::ENOSPC,
            Unsupported => LinuxError::ENOSYS,
            UnexpectedEof | WriteZero => LinuxError::EIO,
            WouldBlock => LinuxError::EAGAIN,
            ArgumentListTooLong => LinuxError::E2BIG,
            CrossesDevices => LinuxError::EXDEV,
            NotATty => LinuxError::ENOTTY,
            NameTooLong => LinuxError::ENAMETOOLONG,
            BadFileDescriptor => LinuxError::EBADF,
            FilesystemLoop => LinuxError::ELOOP,
            OperationNotSupported => LinuxError::EOPNOTSUPP,
            InProgress => LinuxError::EINPROGRESS,
            AlreadyConnected => LinuxError::EISCONN,
            Interrupted => LinuxError::EINTR,
            TimedOut => LinuxError::ETIMEDOUT,
            NotASocket => LinuxError::ENOTSOCK,
            BrokenPipe => LinuxError::EPIPE,
            TooManyOpenFiles => LinuxError::EMFILE,
            NoSuchProcess => LinuxError::ESRCH,
            IllegalBytes => LinuxError::EILSEQ,
            OperationNotPermitted => LinuxError::EPERM,
            OutOfRange => LinuxError::ERANGE,
            InvalidExecutable => LinuxError::ENOEXEC,
            NoSuchDevice => LinuxError::ENODEV,
            ReadOnlyFilesystem => LinuxError::EROFS,
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
            EEXIST => AlreadyExists,
            EFAULT => BadAddress,
            ECONNREFUSED => ConnectionRefused,
            ECONNRESET => ConnectionReset,
            ENOTEMPTY => DirectoryNotEmpty,
            EINVAL => InvalidInput,
            EIO => Io,
            EISDIR => IsADirectory,
            ENOMEM => NoMemory,
            ENOTDIR => NotADirectory,
            ENOTCONN => NotConnected,
            ENOENT => NotFound,
            EACCES => PermissionDenied,
            EBUSY => ResourceBusy,
            ENOSPC => StorageFull,
            ENOSYS => Unsupported,
            EAGAIN => WouldBlock,
            E2BIG => ArgumentListTooLong,
            EXDEV => CrossesDevices,
            ENOTTY => NotATty,
            ENAMETOOLONG => NameTooLong,
            EBADF => BadFileDescriptor,
            ELOOP => FilesystemLoop,
            EOPNOTSUPP => OperationNotSupported,
            EINPROGRESS => InProgress,
            EISCONN => AlreadyConnected,
            EINTR => Interrupted,
            ETIMEDOUT => TimedOut,
            ENOTSOCK => NotASocket,
            EPIPE => BrokenPipe,
            EMFILE => TooManyOpenFiles,
            ESRCH => NoSuchProcess,
            EILSEQ => IllegalBytes,
            EPERM => OperationNotPermitted,
            ERANGE => OutOfRange,
            ENOEXEC => InvalidExecutable,
            ENODEV => NoSuchDevice,
            EROFS => ReadOnlyFilesystem,
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
    use crate::{AxError, LinuxError};

    #[test]
    fn test_try_from() {
        let max_code = core::mem::variant_count::<AxError>() as i32;
        assert_eq!(max_code, 22);
        assert_eq!(max_code, AxError::WriteZero.code());

        assert_eq!(AxError::AddrInUse.code(), 1);
        assert_eq!(Ok(AxError::AddrInUse), AxError::try_from(1));
        assert_eq!(Ok(AxError::AlreadyExists), AxError::try_from(2));
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
