//! Linux errno values copied from Linux 6.6.98 UAPI headers.

use core::fmt;

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Errno {
    /// Operation not permitted
    EPERM           = 1,
    /// No such file or directory
    ENOENT          = 2,
    /// No such process
    ESRCH           = 3,
    /// Interrupted system call
    EINTR           = 4,
    /// I/O error
    EIO             = 5,
    /// No such device or address
    ENXIO           = 6,
    /// Argument list too long
    E2BIG           = 7,
    /// Exec format error
    ENOEXEC         = 8,
    /// Bad file number
    EBADF           = 9,
    /// No child processes
    ECHILD          = 10,
    /// Try again
    EAGAIN          = 11,
    /// Out of memory
    ENOMEM          = 12,
    /// Permission denied
    EACCES          = 13,
    /// Bad address
    EFAULT          = 14,
    /// Block device required
    ENOTBLK         = 15,
    /// Device or resource busy
    EBUSY           = 16,
    /// File exists
    EEXIST          = 17,
    /// Cross-device link
    EXDEV           = 18,
    /// No such device
    ENODEV          = 19,
    /// Not a directory
    ENOTDIR         = 20,
    /// Is a directory
    EISDIR          = 21,
    /// Invalid argument
    EINVAL          = 22,
    /// File table overflow
    ENFILE          = 23,
    /// Too many open files
    EMFILE          = 24,
    /// Not a typewriter
    ENOTTY          = 25,
    /// Text file busy
    ETXTBSY         = 26,
    /// File too large
    EFBIG           = 27,
    /// No space left on device
    ENOSPC          = 28,
    /// Illegal seek
    ESPIPE          = 29,
    /// Read-only file system
    EROFS           = 30,
    /// Too many links
    EMLINK          = 31,
    /// Broken pipe
    EPIPE           = 32,
    /// Math argument out of domain of func
    EDOM            = 33,
    /// Math result not representable
    ERANGE          = 34,
    /// Resource deadlock would occur
    EDEADLK         = 35,
    /// File name too long
    ENAMETOOLONG    = 36,
    /// No record locks available
    ENOLCK          = 37,
    /// Invalid system call number
    ENOSYS          = 38,
    /// Directory not empty
    ENOTEMPTY       = 39,
    /// Too many symbolic links encountered
    ELOOP           = 40,
    /// No message of desired type
    ENOMSG          = 42,
    /// Identifier removed
    EIDRM           = 43,
    /// Channel number out of range
    ECHRNG          = 44,
    /// Level 2 not synchronized
    EL2NSYNC        = 45,
    /// Level 3 halted
    EL3HLT          = 46,
    /// Level 3 reset
    EL3RST          = 47,
    /// Link number out of range
    ELNRNG          = 48,
    /// Protocol driver not attached
    EUNATCH         = 49,
    /// No CSI structure available
    ENOCSI          = 50,
    /// Level 2 halted
    EL2HLT          = 51,
    /// Invalid exchange
    EBADE           = 52,
    /// Invalid request descriptor
    EBADR           = 53,
    /// Exchange full
    EXFULL          = 54,
    /// No anode
    ENOANO          = 55,
    /// Invalid request code
    EBADRQC         = 56,
    /// Invalid slot
    EBADSLT         = 57,
    /// Bad font file format
    EBFONT          = 59,
    /// Device not a stream
    ENOSTR          = 60,
    /// No data available
    ENODATA         = 61,
    /// Timer expired
    ETIME           = 62,
    /// Out of streams resources
    ENOSR           = 63,
    /// Machine is not on the network
    ENONET          = 64,
    /// Package not installed
    ENOPKG          = 65,
    /// Object is remote
    EREMOTE         = 66,
    /// Link has been severed
    ENOLINK         = 67,
    /// Advertise error
    EADV            = 68,
    /// Srmount error
    ESRMNT          = 69,
    /// Communication error on send
    ECOMM           = 70,
    /// Protocol error
    EPROTO          = 71,
    /// Multihop attempted
    EMULTIHOP       = 72,
    /// RFS specific error
    EDOTDOT         = 73,
    /// Not a data message
    EBADMSG         = 74,
    /// Value too large for defined data type
    EOVERFLOW       = 75,
    /// Name not unique on network
    ENOTUNIQ        = 76,
    /// File descriptor in bad state
    EBADFD          = 77,
    /// Remote address changed
    EREMCHG         = 78,
    /// Can not access a needed shared library
    ELIBACC         = 79,
    /// Accessing a corrupted shared library
    ELIBBAD         = 80,
    /// .lib section in a.out corrupted
    ELIBSCN         = 81,
    /// Attempting to link in too many shared libraries
    ELIBMAX         = 82,
    /// Cannot exec a shared library directly
    ELIBEXEC        = 83,
    /// Illegal byte sequence
    EILSEQ          = 84,
    /// Interrupted system call should be restarted
    ERESTART        = 85,
    /// Streams pipe error
    ESTRPIPE        = 86,
    /// Too many users
    EUSERS          = 87,
    /// Socket operation on non-socket
    ENOTSOCK        = 88,
    /// Destination address required
    EDESTADDRREQ    = 89,
    /// Message too long
    EMSGSIZE        = 90,
    /// Protocol wrong type for socket
    EPROTOTYPE      = 91,
    /// Protocol not available
    ENOPROTOOPT     = 92,
    /// Protocol not supported
    EPROTONOSUPPORT = 93,
    /// Socket type not supported
    ESOCKTNOSUPPORT = 94,
    /// Operation not supported on transport endpoint
    EOPNOTSUPP      = 95,
    /// Protocol family not supported
    EPFNOSUPPORT    = 96,
    /// Address family not supported by protocol
    EAFNOSUPPORT    = 97,
    /// Address already in use
    EADDRINUSE      = 98,
    /// Cannot assign requested address
    EADDRNOTAVAIL   = 99,
    /// Network is down
    ENETDOWN        = 100,
    /// Network is unreachable
    ENETUNREACH     = 101,
    /// Network dropped connection because of reset
    ENETRESET       = 102,
    /// Software caused connection abort
    ECONNABORTED    = 103,
    /// Connection reset by peer
    ECONNRESET      = 104,
    /// No buffer space available
    ENOBUFS         = 105,
    /// Transport endpoint is already connected
    EISCONN         = 106,
    /// Transport endpoint is not connected
    ENOTCONN        = 107,
    /// Cannot send after transport endpoint shutdown
    ESHUTDOWN       = 108,
    /// Too many references: cannot splice
    ETOOMANYREFS    = 109,
    /// Connection timed out
    ETIMEDOUT       = 110,
    /// Connection refused
    ECONNREFUSED    = 111,
    /// Host is down
    EHOSTDOWN       = 112,
    /// No route to host
    EHOSTUNREACH    = 113,
    /// Operation already in progress
    EALREADY        = 114,
    /// Operation now in progress
    EINPROGRESS     = 115,
    /// Stale file handle
    ESTALE          = 116,
    /// Structure needs cleaning
    EUCLEAN         = 117,
    /// Not a XENIX named type file
    ENOTNAM         = 118,
    /// No XENIX semaphores available
    ENAVAIL         = 119,
    /// Is a named type file
    EISNAM          = 120,
    /// Remote I/O error
    EREMOTEIO       = 121,
    /// Quota exceeded
    EDQUOT          = 122,
    /// No medium found
    ENOMEDIUM       = 123,
    /// Wrong medium type
    EMEDIUMTYPE     = 124,
    /// Operation Canceled
    ECANCELED       = 125,
    /// Required key not available
    ENOKEY          = 126,
    /// Key has expired
    EKEYEXPIRED     = 127,
    /// Key has been revoked
    EKEYREVOKED     = 128,
    /// Key was rejected by service
    EKEYREJECTED    = 129,
    /// Owner died
    EOWNERDEAD      = 130,
    /// State not recoverable
    ENOTRECOVERABLE = 131,
    /// Operation not possible due to RF-kill
    ERFKILL         = 132,
    /// Memory page has hardware error
    EHWPOISON       = 133,
}

impl Errno {
    pub const EWOULDBLOCK: Self = Self::EAGAIN;

    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::EPERM => "EPERM",
            Self::ENOENT => "ENOENT",
            Self::ESRCH => "ESRCH",
            Self::EINTR => "EINTR",
            Self::EIO => "EIO",
            Self::ENXIO => "ENXIO",
            Self::E2BIG => "E2BIG",
            Self::ENOEXEC => "ENOEXEC",
            Self::EBADF => "EBADF",
            Self::ECHILD => "ECHILD",
            Self::EAGAIN => "EAGAIN",
            Self::ENOMEM => "ENOMEM",
            Self::EACCES => "EACCES",
            Self::EFAULT => "EFAULT",
            Self::ENOTBLK => "ENOTBLK",
            Self::EBUSY => "EBUSY",
            Self::EEXIST => "EEXIST",
            Self::EXDEV => "EXDEV",
            Self::ENODEV => "ENODEV",
            Self::ENOTDIR => "ENOTDIR",
            Self::EISDIR => "EISDIR",
            Self::EINVAL => "EINVAL",
            Self::ENFILE => "ENFILE",
            Self::EMFILE => "EMFILE",
            Self::ENOTTY => "ENOTTY",
            Self::ETXTBSY => "ETXTBSY",
            Self::EFBIG => "EFBIG",
            Self::ENOSPC => "ENOSPC",
            Self::ESPIPE => "ESPIPE",
            Self::EROFS => "EROFS",
            Self::EMLINK => "EMLINK",
            Self::EPIPE => "EPIPE",
            Self::EDOM => "EDOM",
            Self::ERANGE => "ERANGE",
            Self::EDEADLK => "EDEADLK",
            Self::ENAMETOOLONG => "ENAMETOOLONG",
            Self::ENOLCK => "ENOLCK",
            Self::ENOSYS => "ENOSYS",
            Self::ENOTEMPTY => "ENOTEMPTY",
            Self::ELOOP => "ELOOP",
            Self::ENOMSG => "ENOMSG",
            Self::EIDRM => "EIDRM",
            Self::ECHRNG => "ECHRNG",
            Self::EL2NSYNC => "EL2NSYNC",
            Self::EL3HLT => "EL3HLT",
            Self::EL3RST => "EL3RST",
            Self::ELNRNG => "ELNRNG",
            Self::EUNATCH => "EUNATCH",
            Self::ENOCSI => "ENOCSI",
            Self::EL2HLT => "EL2HLT",
            Self::EBADE => "EBADE",
            Self::EBADR => "EBADR",
            Self::EXFULL => "EXFULL",
            Self::ENOANO => "ENOANO",
            Self::EBADRQC => "EBADRQC",
            Self::EBADSLT => "EBADSLT",
            Self::EBFONT => "EBFONT",
            Self::ENOSTR => "ENOSTR",
            Self::ENODATA => "ENODATA",
            Self::ETIME => "ETIME",
            Self::ENOSR => "ENOSR",
            Self::ENONET => "ENONET",
            Self::ENOPKG => "ENOPKG",
            Self::EREMOTE => "EREMOTE",
            Self::ENOLINK => "ENOLINK",
            Self::EADV => "EADV",
            Self::ESRMNT => "ESRMNT",
            Self::ECOMM => "ECOMM",
            Self::EPROTO => "EPROTO",
            Self::EMULTIHOP => "EMULTIHOP",
            Self::EDOTDOT => "EDOTDOT",
            Self::EBADMSG => "EBADMSG",
            Self::EOVERFLOW => "EOVERFLOW",
            Self::ENOTUNIQ => "ENOTUNIQ",
            Self::EBADFD => "EBADFD",
            Self::EREMCHG => "EREMCHG",
            Self::ELIBACC => "ELIBACC",
            Self::ELIBBAD => "ELIBBAD",
            Self::ELIBSCN => "ELIBSCN",
            Self::ELIBMAX => "ELIBMAX",
            Self::ELIBEXEC => "ELIBEXEC",
            Self::EILSEQ => "EILSEQ",
            Self::ERESTART => "ERESTART",
            Self::ESTRPIPE => "ESTRPIPE",
            Self::EUSERS => "EUSERS",
            Self::ENOTSOCK => "ENOTSOCK",
            Self::EDESTADDRREQ => "EDESTADDRREQ",
            Self::EMSGSIZE => "EMSGSIZE",
            Self::EPROTOTYPE => "EPROTOTYPE",
            Self::ENOPROTOOPT => "ENOPROTOOPT",
            Self::EPROTONOSUPPORT => "EPROTONOSUPPORT",
            Self::ESOCKTNOSUPPORT => "ESOCKTNOSUPPORT",
            Self::EOPNOTSUPP => "EOPNOTSUPP",
            Self::EPFNOSUPPORT => "EPFNOSUPPORT",
            Self::EAFNOSUPPORT => "EAFNOSUPPORT",
            Self::EADDRINUSE => "EADDRINUSE",
            Self::EADDRNOTAVAIL => "EADDRNOTAVAIL",
            Self::ENETDOWN => "ENETDOWN",
            Self::ENETUNREACH => "ENETUNREACH",
            Self::ENETRESET => "ENETRESET",
            Self::ECONNABORTED => "ECONNABORTED",
            Self::ECONNRESET => "ECONNRESET",
            Self::ENOBUFS => "ENOBUFS",
            Self::EISCONN => "EISCONN",
            Self::ENOTCONN => "ENOTCONN",
            Self::ESHUTDOWN => "ESHUTDOWN",
            Self::ETOOMANYREFS => "ETOOMANYREFS",
            Self::ETIMEDOUT => "ETIMEDOUT",
            Self::ECONNREFUSED => "ECONNREFUSED",
            Self::EHOSTDOWN => "EHOSTDOWN",
            Self::EHOSTUNREACH => "EHOSTUNREACH",
            Self::EALREADY => "EALREADY",
            Self::EINPROGRESS => "EINPROGRESS",
            Self::ESTALE => "ESTALE",
            Self::EUCLEAN => "EUCLEAN",
            Self::ENOTNAM => "ENOTNAM",
            Self::ENAVAIL => "ENAVAIL",
            Self::EISNAM => "EISNAM",
            Self::EREMOTEIO => "EREMOTEIO",
            Self::EDQUOT => "EDQUOT",
            Self::ENOMEDIUM => "ENOMEDIUM",
            Self::EMEDIUMTYPE => "EMEDIUMTYPE",
            Self::ECANCELED => "ECANCELED",
            Self::ENOKEY => "ENOKEY",
            Self::EKEYEXPIRED => "EKEYEXPIRED",
            Self::EKEYREVOKED => "EKEYREVOKED",
            Self::EKEYREJECTED => "EKEYREJECTED",
            Self::EOWNERDEAD => "EOWNERDEAD",
            Self::ENOTRECOVERABLE => "ENOTRECOVERABLE",
            Self::ERFKILL => "ERFKILL",
            Self::EHWPOISON => "EHWPOISON",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::EPERM => "Operation not permitted",
            Self::ENOENT => "No such file or directory",
            Self::ESRCH => "No such process",
            Self::EINTR => "Interrupted system call",
            Self::EIO => "I/O error",
            Self::ENXIO => "No such device or address",
            Self::E2BIG => "Argument list too long",
            Self::ENOEXEC => "Exec format error",
            Self::EBADF => "Bad file number",
            Self::ECHILD => "No child processes",
            Self::EAGAIN => "Try again",
            Self::ENOMEM => "Out of memory",
            Self::EACCES => "Permission denied",
            Self::EFAULT => "Bad address",
            Self::ENOTBLK => "Block device required",
            Self::EBUSY => "Device or resource busy",
            Self::EEXIST => "File exists",
            Self::EXDEV => "Cross-device link",
            Self::ENODEV => "No such device",
            Self::ENOTDIR => "Not a directory",
            Self::EISDIR => "Is a directory",
            Self::EINVAL => "Invalid argument",
            Self::ENFILE => "File table overflow",
            Self::EMFILE => "Too many open files",
            Self::ENOTTY => "Not a typewriter",
            Self::ETXTBSY => "Text file busy",
            Self::EFBIG => "File too large",
            Self::ENOSPC => "No space left on device",
            Self::ESPIPE => "Illegal seek",
            Self::EROFS => "Read-only file system",
            Self::EMLINK => "Too many links",
            Self::EPIPE => "Broken pipe",
            Self::EDOM => "Math argument out of domain of func",
            Self::ERANGE => "Math result not representable",
            Self::EDEADLK => "Resource deadlock would occur",
            Self::ENAMETOOLONG => "File name too long",
            Self::ENOLCK => "No record locks available",
            Self::ENOSYS => "Invalid system call number",
            Self::ENOTEMPTY => "Directory not empty",
            Self::ELOOP => "Too many symbolic links encountered",
            Self::ENOMSG => "No message of desired type",
            Self::EIDRM => "Identifier removed",
            Self::ECHRNG => "Channel number out of range",
            Self::EL2NSYNC => "Level 2 not synchronized",
            Self::EL3HLT => "Level 3 halted",
            Self::EL3RST => "Level 3 reset",
            Self::ELNRNG => "Link number out of range",
            Self::EUNATCH => "Protocol driver not attached",
            Self::ENOCSI => "No CSI structure available",
            Self::EL2HLT => "Level 2 halted",
            Self::EBADE => "Invalid exchange",
            Self::EBADR => "Invalid request descriptor",
            Self::EXFULL => "Exchange full",
            Self::ENOANO => "No anode",
            Self::EBADRQC => "Invalid request code",
            Self::EBADSLT => "Invalid slot",
            Self::EBFONT => "Bad font file format",
            Self::ENOSTR => "Device not a stream",
            Self::ENODATA => "No data available",
            Self::ETIME => "Timer expired",
            Self::ENOSR => "Out of streams resources",
            Self::ENONET => "Machine is not on the network",
            Self::ENOPKG => "Package not installed",
            Self::EREMOTE => "Object is remote",
            Self::ENOLINK => "Link has been severed",
            Self::EADV => "Advertise error",
            Self::ESRMNT => "Srmount error",
            Self::ECOMM => "Communication error on send",
            Self::EPROTO => "Protocol error",
            Self::EMULTIHOP => "Multihop attempted",
            Self::EDOTDOT => "RFS specific error",
            Self::EBADMSG => "Not a data message",
            Self::EOVERFLOW => "Value too large for defined data type",
            Self::ENOTUNIQ => "Name not unique on network",
            Self::EBADFD => "File descriptor in bad state",
            Self::EREMCHG => "Remote address changed",
            Self::ELIBACC => "Can not access a needed shared library",
            Self::ELIBBAD => "Accessing a corrupted shared library",
            Self::ELIBSCN => ".lib section in a.out corrupted",
            Self::ELIBMAX => "Attempting to link in too many shared libraries",
            Self::ELIBEXEC => "Cannot exec a shared library directly",
            Self::EILSEQ => "Illegal byte sequence",
            Self::ERESTART => "Interrupted system call should be restarted",
            Self::ESTRPIPE => "Streams pipe error",
            Self::EUSERS => "Too many users",
            Self::ENOTSOCK => "Socket operation on non-socket",
            Self::EDESTADDRREQ => "Destination address required",
            Self::EMSGSIZE => "Message too long",
            Self::EPROTOTYPE => "Protocol wrong type for socket",
            Self::ENOPROTOOPT => "Protocol not available",
            Self::EPROTONOSUPPORT => "Protocol not supported",
            Self::ESOCKTNOSUPPORT => "Socket type not supported",
            Self::EOPNOTSUPP => "Operation not supported on transport endpoint",
            Self::EPFNOSUPPORT => "Protocol family not supported",
            Self::EAFNOSUPPORT => "Address family not supported by protocol",
            Self::EADDRINUSE => "Address already in use",
            Self::EADDRNOTAVAIL => "Cannot assign requested address",
            Self::ENETDOWN => "Network is down",
            Self::ENETUNREACH => "Network is unreachable",
            Self::ENETRESET => "Network dropped connection because of reset",
            Self::ECONNABORTED => "Software caused connection abort",
            Self::ECONNRESET => "Connection reset by peer",
            Self::ENOBUFS => "No buffer space available",
            Self::EISCONN => "Transport endpoint is already connected",
            Self::ENOTCONN => "Transport endpoint is not connected",
            Self::ESHUTDOWN => "Cannot send after transport endpoint shutdown",
            Self::ETOOMANYREFS => "Too many references: cannot splice",
            Self::ETIMEDOUT => "Connection timed out",
            Self::ECONNREFUSED => "Connection refused",
            Self::EHOSTDOWN => "Host is down",
            Self::EHOSTUNREACH => "No route to host",
            Self::EALREADY => "Operation already in progress",
            Self::EINPROGRESS => "Operation now in progress",
            Self::ESTALE => "Stale file handle",
            Self::EUCLEAN => "Structure needs cleaning",
            Self::ENOTNAM => "Not a XENIX named type file",
            Self::ENAVAIL => "No XENIX semaphores available",
            Self::EISNAM => "Is a named type file",
            Self::EREMOTEIO => "Remote I/O error",
            Self::EDQUOT => "Quota exceeded",
            Self::ENOMEDIUM => "No medium found",
            Self::EMEDIUMTYPE => "Wrong medium type",
            Self::ECANCELED => "Operation Canceled",
            Self::ENOKEY => "Required key not available",
            Self::EKEYEXPIRED => "Key has expired",
            Self::EKEYREVOKED => "Key has been revoked",
            Self::EKEYREJECTED => "Key was rejected by service",
            Self::EOWNERDEAD => "Owner died",
            Self::ENOTRECOVERABLE => "State not recoverable",
            Self::ERFKILL => "Operation not possible due to RF-kill",
            Self::EHWPOISON => "Memory page has hardware error",
        }
    }

    pub const fn from_i32(code: i32) -> Option<Self> {
        match code {
            1 => Some(Self::EPERM),
            2 => Some(Self::ENOENT),
            3 => Some(Self::ESRCH),
            4 => Some(Self::EINTR),
            5 => Some(Self::EIO),
            6 => Some(Self::ENXIO),
            7 => Some(Self::E2BIG),
            8 => Some(Self::ENOEXEC),
            9 => Some(Self::EBADF),
            10 => Some(Self::ECHILD),
            11 => Some(Self::EAGAIN),
            12 => Some(Self::ENOMEM),
            13 => Some(Self::EACCES),
            14 => Some(Self::EFAULT),
            15 => Some(Self::ENOTBLK),
            16 => Some(Self::EBUSY),
            17 => Some(Self::EEXIST),
            18 => Some(Self::EXDEV),
            19 => Some(Self::ENODEV),
            20 => Some(Self::ENOTDIR),
            21 => Some(Self::EISDIR),
            22 => Some(Self::EINVAL),
            23 => Some(Self::ENFILE),
            24 => Some(Self::EMFILE),
            25 => Some(Self::ENOTTY),
            26 => Some(Self::ETXTBSY),
            27 => Some(Self::EFBIG),
            28 => Some(Self::ENOSPC),
            29 => Some(Self::ESPIPE),
            30 => Some(Self::EROFS),
            31 => Some(Self::EMLINK),
            32 => Some(Self::EPIPE),
            33 => Some(Self::EDOM),
            34 => Some(Self::ERANGE),
            35 => Some(Self::EDEADLK),
            36 => Some(Self::ENAMETOOLONG),
            37 => Some(Self::ENOLCK),
            38 => Some(Self::ENOSYS),
            39 => Some(Self::ENOTEMPTY),
            40 => Some(Self::ELOOP),
            42 => Some(Self::ENOMSG),
            43 => Some(Self::EIDRM),
            44 => Some(Self::ECHRNG),
            45 => Some(Self::EL2NSYNC),
            46 => Some(Self::EL3HLT),
            47 => Some(Self::EL3RST),
            48 => Some(Self::ELNRNG),
            49 => Some(Self::EUNATCH),
            50 => Some(Self::ENOCSI),
            51 => Some(Self::EL2HLT),
            52 => Some(Self::EBADE),
            53 => Some(Self::EBADR),
            54 => Some(Self::EXFULL),
            55 => Some(Self::ENOANO),
            56 => Some(Self::EBADRQC),
            57 => Some(Self::EBADSLT),
            59 => Some(Self::EBFONT),
            60 => Some(Self::ENOSTR),
            61 => Some(Self::ENODATA),
            62 => Some(Self::ETIME),
            63 => Some(Self::ENOSR),
            64 => Some(Self::ENONET),
            65 => Some(Self::ENOPKG),
            66 => Some(Self::EREMOTE),
            67 => Some(Self::ENOLINK),
            68 => Some(Self::EADV),
            69 => Some(Self::ESRMNT),
            70 => Some(Self::ECOMM),
            71 => Some(Self::EPROTO),
            72 => Some(Self::EMULTIHOP),
            73 => Some(Self::EDOTDOT),
            74 => Some(Self::EBADMSG),
            75 => Some(Self::EOVERFLOW),
            76 => Some(Self::ENOTUNIQ),
            77 => Some(Self::EBADFD),
            78 => Some(Self::EREMCHG),
            79 => Some(Self::ELIBACC),
            80 => Some(Self::ELIBBAD),
            81 => Some(Self::ELIBSCN),
            82 => Some(Self::ELIBMAX),
            83 => Some(Self::ELIBEXEC),
            84 => Some(Self::EILSEQ),
            85 => Some(Self::ERESTART),
            86 => Some(Self::ESTRPIPE),
            87 => Some(Self::EUSERS),
            88 => Some(Self::ENOTSOCK),
            89 => Some(Self::EDESTADDRREQ),
            90 => Some(Self::EMSGSIZE),
            91 => Some(Self::EPROTOTYPE),
            92 => Some(Self::ENOPROTOOPT),
            93 => Some(Self::EPROTONOSUPPORT),
            94 => Some(Self::ESOCKTNOSUPPORT),
            95 => Some(Self::EOPNOTSUPP),
            96 => Some(Self::EPFNOSUPPORT),
            97 => Some(Self::EAFNOSUPPORT),
            98 => Some(Self::EADDRINUSE),
            99 => Some(Self::EADDRNOTAVAIL),
            100 => Some(Self::ENETDOWN),
            101 => Some(Self::ENETUNREACH),
            102 => Some(Self::ENETRESET),
            103 => Some(Self::ECONNABORTED),
            104 => Some(Self::ECONNRESET),
            105 => Some(Self::ENOBUFS),
            106 => Some(Self::EISCONN),
            107 => Some(Self::ENOTCONN),
            108 => Some(Self::ESHUTDOWN),
            109 => Some(Self::ETOOMANYREFS),
            110 => Some(Self::ETIMEDOUT),
            111 => Some(Self::ECONNREFUSED),
            112 => Some(Self::EHOSTDOWN),
            113 => Some(Self::EHOSTUNREACH),
            114 => Some(Self::EALREADY),
            115 => Some(Self::EINPROGRESS),
            116 => Some(Self::ESTALE),
            117 => Some(Self::EUCLEAN),
            118 => Some(Self::ENOTNAM),
            119 => Some(Self::ENAVAIL),
            120 => Some(Self::EISNAM),
            121 => Some(Self::EREMOTEIO),
            122 => Some(Self::EDQUOT),
            123 => Some(Self::ENOMEDIUM),
            124 => Some(Self::EMEDIUMTYPE),
            125 => Some(Self::ECANCELED),
            126 => Some(Self::ENOKEY),
            127 => Some(Self::EKEYEXPIRED),
            128 => Some(Self::EKEYREVOKED),
            129 => Some(Self::EKEYREJECTED),
            130 => Some(Self::EOWNERDEAD),
            131 => Some(Self::ENOTRECOVERABLE),
            132 => Some(Self::ERFKILL),
            133 => Some(Self::EHWPOISON),
            _ => None,
        }
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.as_i32())
    }
}
