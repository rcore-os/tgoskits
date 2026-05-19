use rdrive::register::DriverRegister;

pub fn builtin_registers() -> &'static [DriverRegister] {
    &[
        #[cfg(feature = "bus-pci")]
        crate::pci::REGISTER,
        #[cfg(feature = "virtio-blk")]
        crate::virtio::block::REGISTER,
        #[cfg(feature = "virtio-net")]
        crate::virtio::net::REGISTER,
        #[cfg(feature = "virtio-gpu")]
        crate::virtio::display::REGISTER,
        #[cfg(feature = "virtio-input")]
        crate::virtio::input::REGISTER,
        #[cfg(feature = "virtio-socket")]
        crate::virtio::vsock::REGISTER,
        #[cfg(feature = "ramdisk")]
        crate::block::ramdisk::REGISTER,
        #[cfg(feature = "sdmmc")]
        crate::block::sdmmc::REGISTER,
        #[cfg(feature = "cvsd")]
        crate::block::cvsd::REGISTER,
        #[cfg(feature = "bcm2835-sdhci")]
        crate::block::bcm2835::REGISTER,
        #[cfg(feature = "ahci")]
        crate::block::ahci::REGISTER,
        #[cfg(feature = "ixgbe")]
        crate::net::ixgbe::REGISTER,
        #[cfg(feature = "fxmac")]
        crate::net::fxmac::REGISTER,
    ]
}

pub fn linker_registers() -> &'static [DriverRegister] {
    let bytes = linker_register_bytes();
    if bytes.is_empty() {
        return &[];
    }

    unsafe {
        core::slice::from_raw_parts(
            bytes.as_ptr().cast::<DriverRegister>(),
            bytes.len() / size_of::<DriverRegister>(),
        )
    }
}

fn linker_register_bytes() -> &'static [u8] {
    unsafe extern "C" {
        fn _sdriver();
        fn _edriver();
    }

    unsafe {
        core::slice::from_raw_parts(
            _sdriver as *const () as *const u8,
            _edriver as *const () as usize - _sdriver as *const () as usize,
        )
    }
}
