use alloc::{format, string::ToString};

use axtest::prelude::*;

use crate as ax_errno;

#[axtest]
fn axerrno_kind_and_linux_mapping_rules_hold() {
    use ax_errno::{AxError, AxErrorKind, LinuxError};

    let mapping = [
        (
            AxErrorKind::AddrInUse,
            LinuxError::EADDRINUSE,
            "Address in use",
        ),
        (
            AxErrorKind::AlreadyConnected,
            LinuxError::EISCONN,
            "Already connected",
        ),
        (
            AxErrorKind::AlreadyExists,
            LinuxError::EEXIST,
            "Entity already exists",
        ),
        (AxErrorKind::BadAddress, LinuxError::EFAULT, "Bad address"),
        (
            AxErrorKind::BadState,
            LinuxError::EFAULT,
            "Bad internal state",
        ),
        (
            AxErrorKind::BadFileDescriptor,
            LinuxError::EBADF,
            "Bad file descriptor",
        ),
        (AxErrorKind::BrokenPipe, LinuxError::EPIPE, "Broken pipe"),
        (
            AxErrorKind::IllegalBytes,
            LinuxError::EILSEQ,
            "Illegal byte sequence",
        ),
        (AxErrorKind::InvalidData, LinuxError::EINVAL, "Invalid data"),
        (
            AxErrorKind::InvalidInput,
            LinuxError::EINVAL,
            "Invalid input parameter",
        ),
        (AxErrorKind::Io, LinuxError::EIO, "I/O error"),
        (AxErrorKind::NoMemory, LinuxError::ENOMEM, "Out of memory"),
        (
            AxErrorKind::NotFound,
            LinuxError::ENOENT,
            "Entity not found",
        ),
        (
            AxErrorKind::OperationNotPermitted,
            LinuxError::EPERM,
            "Operation not permitted",
        ),
        (
            AxErrorKind::OperationNotSupported,
            LinuxError::EOPNOTSUPP,
            "Operation not supported",
        ),
        (
            AxErrorKind::PermissionDenied,
            LinuxError::EACCES,
            "Permission denied",
        ),
        (
            AxErrorKind::StorageFull,
            LinuxError::ENOSPC,
            "No storage space",
        ),
        (
            AxErrorKind::UnexpectedEof,
            LinuxError::EIO,
            "Unexpected end of file",
        ),
        (
            AxErrorKind::Unsupported,
            LinuxError::ENOSYS,
            "Operation not supported",
        ),
        (
            AxErrorKind::WouldBlock,
            LinuxError::EAGAIN,
            "Operation would block",
        ),
        (AxErrorKind::WriteZero, LinuxError::EIO, "Write zero"),
    ];

    for (kind, linux, text) in mapping {
        ax_assert_eq!(LinuxError::from(kind), linux);
        ax_assert_eq!(kind.as_str(), text);
        ax_assert_eq!(kind.to_string(), text);
        ax_assert_eq!(AxErrorKind::try_from(kind.code()), Ok(kind));
        ax_assert_eq!(AxError::from(kind).canonicalize(), AxError::from(kind));
    }

    ax_assert!(AxErrorKind::try_from(0).is_err());
    ax_assert!(AxErrorKind::try_from(i32::MAX).is_err());
}

#[axtest]
fn axerrno_axerror_conversion_and_formatting_rules_hold() {
    use ax_errno::{AxError, AxErrorKind, LinuxError};

    let permission = AxError::from(AxErrorKind::PermissionDenied);
    ax_assert_eq!(permission.code(), AxErrorKind::PermissionDenied.code());
    ax_assert_eq!(LinuxError::from(permission), LinuxError::EACCES);
    ax_assert_eq!(
        AxErrorKind::try_from(permission),
        Ok(AxErrorKind::PermissionDenied)
    );
    ax_assert!(format!("{permission:?}").contains("AxErrorKind::PermissionDenied"));
    ax_assert_eq!(permission.to_string(), "Permission denied");

    let linux = AxError::from(LinuxError::EACCES);
    ax_assert_eq!(linux.code(), -LinuxError::EACCES.code());
    ax_assert_eq!(LinuxError::from(linux), LinuxError::EACCES);
    ax_assert_eq!(
        AxErrorKind::try_from(linux),
        Ok(AxErrorKind::PermissionDenied)
    );
    ax_assert_eq!(
        linux.canonicalize(),
        AxError::from(AxErrorKind::PermissionDenied)
    );
    ax_assert!(format!("{linux:?}").contains("LinuxError::EACCES"));
    ax_assert_eq!(linux.to_string(), LinuxError::EACCES.as_str());

    let unknown_linux = AxError::from(LinuxError::ENOMEDIUM);
    ax_assert_eq!(LinuxError::from(unknown_linux), LinuxError::ENOMEDIUM);
    ax_assert_eq!(
        AxErrorKind::try_from(unknown_linux),
        Err(LinuxError::ENOMEDIUM)
    );
    ax_assert_eq!(unknown_linux.canonicalize(), unknown_linux);

    ax_assert_eq!(
        AxError::try_from(AxErrorKind::NotFound.code()),
        Ok(AxError::NotFound)
    );
    ax_assert_eq!(
        AxError::try_from(-LinuxError::ENOENT.code()),
        Ok(AxError::from(LinuxError::ENOENT))
    );
    ax_assert!(AxError::try_from(i32::MAX).is_err());
    ax_assert_eq!(
        AxError::from(core::fmt::Error),
        AxError::from(AxErrorKind::InvalidInput)
    );
}

#[axtest]
fn axerrno_linux_error_roundtrip_rules_hold() {
    use ax_errno::{AxErrorKind, LinuxError};

    let roundtrip = [
        LinuxError::EADDRINUSE,
        LinuxError::EISCONN,
        LinuxError::EEXIST,
        LinuxError::E2BIG,
        LinuxError::EFAULT,
        LinuxError::EBADF,
        LinuxError::EPIPE,
        LinuxError::ECONNREFUSED,
        LinuxError::ECONNRESET,
        LinuxError::EXDEV,
        LinuxError::ENOTEMPTY,
        LinuxError::ELOOP,
        LinuxError::EILSEQ,
        LinuxError::EINPROGRESS,
        LinuxError::EINTR,
        LinuxError::ENOEXEC,
        LinuxError::EINVAL,
        LinuxError::EIO,
        LinuxError::EISDIR,
        LinuxError::ENAMETOOLONG,
        LinuxError::ENOMEM,
        LinuxError::ENODEV,
        LinuxError::ENXIO,
        LinuxError::ESRCH,
        LinuxError::ENOTDIR,
        LinuxError::ENOTSOCK,
        LinuxError::ENOTTY,
        LinuxError::EDESTADDRREQ,
        LinuxError::EMSGSIZE,
        LinuxError::ENOTCONN,
        LinuxError::ENOENT,
        LinuxError::EPERM,
        LinuxError::EOPNOTSUPP,
        LinuxError::ERANGE,
        LinuxError::EACCES,
        LinuxError::EROFS,
        LinuxError::EBUSY,
        LinuxError::ENOSPC,
        LinuxError::ETIMEDOUT,
        LinuxError::EMFILE,
        LinuxError::ENOSYS,
        LinuxError::EAGAIN,
    ];

    for linux in roundtrip {
        let kind = AxErrorKind::try_from(linux).unwrap();
        ax_assert_eq!(LinuxError::from(kind), linux);
        ax_assert_eq!(LinuxError::try_from(linux.code()), Ok(linux));
        ax_assert_eq!(linux.to_string(), linux.as_str());
    }

    ax_assert!(LinuxError::try_from(0).is_err());
    ax_assert!(LinuxError::try_from(i32::MAX).is_err());
    ax_assert_eq!(
        AxErrorKind::try_from(LinuxError::ENOMEDIUM),
        Err(LinuxError::ENOMEDIUM)
    );
}

#[axtest]
fn axerrno_macros_return_expected_errors() {
    use ax_errno::{AxError, AxResult, ax_bail, ax_err, ax_err_type, ensure};

    fn ensure_positive(value: isize) -> AxResult<usize> {
        ensure!(value > 0, ax_err!(InvalidInput));
        Ok(value as usize)
    }

    fn bail_now() -> AxResult {
        ax_bail!(PermissionDenied, "permission denied by coverage test");
    }

    ax_assert_eq!(ax_err_type!(AlreadyExists), AxError::AlreadyExists);
    ax_assert_eq!(
        ax_err_type!(BadAddress, "bad address by coverage test"),
        AxError::BadAddress
    );
    ax_assert_eq!(ax_err!(NotFound), AxResult::<()>::Err(AxError::NotFound));
    ax_assert_eq!(
        ax_err!(StorageFull, "storage full by coverage test"),
        AxResult::<()>::Err(AxError::StorageFull)
    );
    ax_assert_eq!(ensure_positive(7), Ok(7));
    ax_assert_eq!(ensure_positive(0), Err(AxError::InvalidInput));
    ax_assert_eq!(bail_now(), Err(AxError::PermissionDenied));
}

#[axtest]
fn axerrno_all_public_constants_match_their_kinds() {
    use ax_errno::{AxError, AxErrorKind, LinuxError};

    let constants = [
        (AxError::AddrInUse, AxErrorKind::AddrInUse),
        (AxError::AlreadyConnected, AxErrorKind::AlreadyConnected),
        (AxError::AlreadyExists, AxErrorKind::AlreadyExists),
        (
            AxError::ArgumentListTooLong,
            AxErrorKind::ArgumentListTooLong,
        ),
        (AxError::BadAddress, AxErrorKind::BadAddress),
        (AxError::BadFileDescriptor, AxErrorKind::BadFileDescriptor),
        (AxError::BadState, AxErrorKind::BadState),
        (AxError::BrokenPipe, AxErrorKind::BrokenPipe),
        (AxError::ConnectionRefused, AxErrorKind::ConnectionRefused),
        (AxError::ConnectionReset, AxErrorKind::ConnectionReset),
        (AxError::CrossesDevices, AxErrorKind::CrossesDevices),
        (AxError::DirectoryNotEmpty, AxErrorKind::DirectoryNotEmpty),
        (AxError::FilesystemLoop, AxErrorKind::FilesystemLoop),
        (AxError::IllegalBytes, AxErrorKind::IllegalBytes),
        (AxError::InProgress, AxErrorKind::InProgress),
        (AxError::Interrupted, AxErrorKind::Interrupted),
        (AxError::InvalidData, AxErrorKind::InvalidData),
        (AxError::InvalidExecutable, AxErrorKind::InvalidExecutable),
        (AxError::InvalidInput, AxErrorKind::InvalidInput),
        (AxError::Io, AxErrorKind::Io),
        (AxError::IsADirectory, AxErrorKind::IsADirectory),
        (AxError::NameTooLong, AxErrorKind::NameTooLong),
        (AxError::NoMemory, AxErrorKind::NoMemory),
        (AxError::NoSuchDevice, AxErrorKind::NoSuchDevice),
        (
            AxError::NoSuchDeviceOrAddress,
            AxErrorKind::NoSuchDeviceOrAddress,
        ),
        (AxError::NoSuchProcess, AxErrorKind::NoSuchProcess),
        (AxError::NotADirectory, AxErrorKind::NotADirectory),
        (AxError::NotASocket, AxErrorKind::NotASocket),
        (AxError::NotATty, AxErrorKind::NotATty),
        (AxError::NotConnected, AxErrorKind::NotConnected),
        (AxError::NotFound, AxErrorKind::NotFound),
        (
            AxError::OperationNotPermitted,
            AxErrorKind::OperationNotPermitted,
        ),
        (
            AxError::OperationNotSupported,
            AxErrorKind::OperationNotSupported,
        ),
        (AxError::OutOfRange, AxErrorKind::OutOfRange),
        (AxError::PermissionDenied, AxErrorKind::PermissionDenied),
        (AxError::ReadOnlyFilesystem, AxErrorKind::ReadOnlyFilesystem),
        (AxError::ResourceBusy, AxErrorKind::ResourceBusy),
        (AxError::StorageFull, AxErrorKind::StorageFull),
        (AxError::TimedOut, AxErrorKind::TimedOut),
        (AxError::TooManyOpenFiles, AxErrorKind::TooManyOpenFiles),
        (AxError::UnexpectedEof, AxErrorKind::UnexpectedEof),
        (AxError::Unsupported, AxErrorKind::Unsupported),
        (AxError::WouldBlock, AxErrorKind::WouldBlock),
        (AxError::DestAddrRequired, AxErrorKind::DestAddrRequired),
        (AxError::MessageTooLong, AxErrorKind::MessageTooLong),
        (AxError::WriteZero, AxErrorKind::WriteZero),
    ];

    for (error, kind) in constants {
        ax_assert_eq!(AxErrorKind::try_from(error), Ok(kind));
        ax_assert_eq!(error.code(), kind.code());
        ax_assert_eq!(LinuxError::from(error), LinuxError::from(kind));
        ax_assert_eq!(format!("{error}"), kind.as_str());
    }
}
