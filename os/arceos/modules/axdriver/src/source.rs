use rdrive::probe::static_::StaticDeviceDesc;

pub static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    #[cfg(all(feature = "bus-mmio", virtio_dev))]
    StaticDeviceDesc::new(crate::virtio::MMIO_DEVICE_NAME),
    #[cfg(feature = "bus-pci")]
    StaticDeviceDesc::new(crate::pci::DEVICE_NAME),
    #[cfg(feature = "ramdisk")]
    StaticDeviceDesc::new(crate::block::ramdisk::DEVICE_NAME),
    #[cfg(feature = "sdmmc")]
    StaticDeviceDesc::new(crate::block::sdmmc::DEVICE_NAME),
    #[cfg(feature = "cvsd")]
    StaticDeviceDesc::new(crate::block::cvsd::DEVICE_NAME),
    #[cfg(feature = "bcm2835-sdhci")]
    StaticDeviceDesc::new(crate::block::bcm2835::DEVICE_NAME),
    #[cfg(feature = "ahci")]
    StaticDeviceDesc::new(crate::block::ahci::DEVICE_NAME),
    #[cfg(feature = "fxmac")]
    StaticDeviceDesc::new(crate::net::fxmac::DEVICE_NAME),
];
