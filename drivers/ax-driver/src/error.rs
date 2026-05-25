#[derive(Debug)]
pub enum Error {
    Driver(rdrive::error::DriverError),
    Probe(rdrive::ProbeError),
}

pub type Result<T = ()> = core::result::Result<T, Error>;

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Driver(err) => write!(f, "driver init failed: {err}"),
            Self::Probe(err) => write!(f, "driver probe failed: {err}"),
        }
    }
}

impl core::error::Error for Error {}

impl From<rdrive::error::DriverError> for Error {
    fn from(value: rdrive::error::DriverError) -> Self {
        Self::Driver(value)
    }
}

impl From<rdrive::ProbeError> for Error {
    fn from(value: rdrive::ProbeError) -> Self {
        Self::Probe(value)
    }
}
