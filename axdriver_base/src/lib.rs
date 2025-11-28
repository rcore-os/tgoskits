//! Device driver interfaces used by [ArceOS][1]. It provides common traits and
//! types for implementing a device driver.
//!
//! You have to use this crate with the following crates for corresponding
//! device types:
//!
//! - [`axdriver_block`][2]: Common traits for block storage drivers.
//! - [`axdriver_display`][3]: Common traits and types for graphics display drivers.
//! - [`axdriver_net`][4]: Common traits and types for network (NIC) drivers.
//!
//! [1]: https://github.com/arceos-org/arceos
//! [2]: ../axdriver_block/index.html
//! [3]: ../axdriver_display/index.html
//! [4]: ../axdriver_net/index.html

#![no_std]

/// All supported device types.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DeviceType {
    /// Block storage device (e.g., disk).
    Block,
    /// Character device (e.g., serial port).
    Char,
    /// Network device (e.g., ethernet card).
    Net,
    /// Graphic display device (e.g., GPU)
    Display,
}

/// The error type for device operation failures.
#[derive(Debug)]
pub enum DevError {
    /// An entity already exists.
    AlreadyExists,
    /// Try again, for non-blocking APIs.
    Again,
    /// Bad internal state.
    BadState,
    /// Invalid parameter/argument.
    InvalidParam,
    /// Input/output error.
    Io,
    /// Not enough space/cannot allocate memory (DMA).
    NoMemory,
    /// Device or resource is busy.
    ResourceBusy,
    /// This operation is unsupported or unimplemented.
    Unsupported,
}

impl core::fmt::Display for DevError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            DevError::AlreadyExists => write!(f, "Entity already exists"),
            DevError::Again => write!(f, "Try again"),
            DevError::BadState => write!(f, "Bad state"),
            DevError::InvalidParam => write!(f, "Invalid parameter"),
            DevError::Io => write!(f, "Input/output error"),
            DevError::NoMemory => write!(f, "Not enough memory"),
            DevError::ResourceBusy => write!(f, "Resource is busy"),
            DevError::Unsupported => write!(f, "Unsupported operation"),
        }
    }
}

/// A specialized `Result` type for device operations.
pub type DevResult<T = ()> = Result<T, DevError>;

/// Common operations that require all device drivers to implement.
pub trait BaseDriverOps: Send + Sync {
    /// The name of the device.
    fn device_name(&self) -> &str;

    /// The type of the device.
    fn device_type(&self) -> DeviceType;
}
