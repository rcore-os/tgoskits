//! AxVM-owned configured VirtIO block devices.

mod device;
#[cfg(feature = "fs")]
mod image;
mod options;

pub(super) fn register(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), crate::ConfiguredDeviceError> {
    catalog.register(module_path!(), device::REGISTRATION)
}
