use crate::{
    PlatformDevice,
    error::DriverError,
    probe::{OnProbeError, ProbeError},
    register::DriverRegister,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiRoot {
    pub rsdp: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiId {
    pub hid: &'static str,
    pub cids: &'static [&'static str],
}

pub struct AcpiInfo<'a> {
    _private: core::marker::PhantomData<&'a ()>,
}

pub type FnOnProbe = fn(AcpiInfo<'_>, PlatformDevice) -> Result<(), OnProbeError>;

pub fn check_root(_root: AcpiRoot) -> Result<(), DriverError> {
    Err(DriverError::Unsupported("acpi"))
}

pub fn init(root: AcpiRoot) -> Result<(), DriverError> {
    check_root(root)
}

pub(crate) fn try_probe_register(
    _register: &DriverRegister,
) -> Option<Result<alloc::vec::Vec<Result<(), OnProbeError>>, ProbeError>> {
    None
}
