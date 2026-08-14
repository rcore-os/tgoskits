/// Errors owned by driver discovery and binding collection.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A registered driver failed to initialize.
    #[error("driver init failed: {0}")]
    Driver(#[from] rdrive::error::DriverError),
    /// Platform device probing failed.
    #[error("driver probe failed: {0}")]
    Probe(#[from] rdrive::ProbeError),
    /// A registered device could not be locked for ownership transfer.
    #[error("registered driver device is busy")]
    DeviceBusy,
    /// A registered device has already transferred its owned interface.
    #[error("registered driver device was already taken")]
    DeviceAlreadyTaken,
    /// A registered device cannot provide its expected interface.
    #[error("registered driver device is unavailable")]
    DeviceUnavailable,
}

/// A result returned by driver discovery and binding collection.
pub type Result<T = ()> = core::result::Result<T, Error>;
