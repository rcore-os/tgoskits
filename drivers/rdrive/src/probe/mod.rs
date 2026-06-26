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
pub mod usb;

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
}
