//! AxVM-owned configurable virtual-device models.

mod ivc;
mod virtio_blk;
mod virtio_net;

#[cfg(test)]
pub(super) use ivc::IVC_CHANNEL_SHARED_RANGE_SIZE;

pub(super) fn register_devices(
    catalog: &mut crate::ConfiguredDeviceCatalog,
) -> Result<(), crate::ConfiguredDeviceError> {
    ivc::register(catalog)?;
    virtio_blk::register(catalog)?;
    virtio_net::register(catalog)
}
