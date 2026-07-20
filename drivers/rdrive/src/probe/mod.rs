use alloc::{
    boxed::Box,
    string::{String, ToString},
};
use core::error::Error;

use fdt_raw::FdtError;

pub mod acpi;
pub mod fdt;
pub mod pci;
pub mod static_;

#[derive(thiserror::Error, Debug)]
pub enum ProbeError {
    #[error("probe `{name}` fail: irq chip not init")]
    IrqNotInit { name: String },
    #[error("fdt parse error: {0}")]
    Fdt(String),
    #[error("on probe error: {0}")]
    OnProbe(#[from] OnProbeError),
    #[error("open device fail")]
    OpenFail(#[from] rdif_base::KError),
    #[error("unsupported probe backend: {0}")]
    Unsupported(&'static str),
}

impl From<FdtError> for ProbeError {
    fn from(value: FdtError) -> Self {
        Self::Fdt(format!("{value:?}"))
    }
}

#[derive(thiserror::Error, Debug)]
pub enum OnProbeError {
    #[error("probe not match")]
    NotMatch,
    #[error("kerror: {0}")]
    KError(#[from] rdif_base::KError),
    #[error("other error: {0}")]
    Other(#[from] Box<dyn Error>),
    #[error("fdt parse error: {0}")]
    Fdt(String),
    #[error("unsupported probe backend: {0}")]
    Unsupported(&'static str),
    /// The driver took exclusive ownership of the probed device but could not
    /// publish it. The probe framework must not offer that device to another
    /// driver because hardware-visible resources may remain quarantined.
    #[error("claimed device failed terminally: {0}")]
    Claimed(Box<dyn Error>),
}

impl From<FdtError> for OnProbeError {
    fn from(value: FdtError) -> Self {
        Self::Fdt(format!("{value:?}"))
    }
}

impl OnProbeError {
    pub fn other(msg: impl AsRef<str>) -> Self {
        Self::Other(msg.as_ref().to_string().into())
    }

    /// Creates a terminal failure for a device whose owner was consumed.
    pub fn claimed(msg: impl AsRef<str>) -> Self {
        Self::Claimed(msg.as_ref().to_string().into())
    }

    /// Returns whether probing must stop because the device is already owned.
    pub const fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed(_))
    }
}
