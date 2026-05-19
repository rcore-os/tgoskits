use rdrive::register::DriverRegister;

pub fn builtin_registers() -> &'static [DriverRegister] {
    &[
        #[cfg(feature = "ramdisk")]
        crate::block::ramdisk::REGISTER,
        #[cfg(feature = "sdmmc")]
        crate::block::sdmmc::REGISTER,
        #[cfg(feature = "cvsd")]
        crate::block::cvsd::REGISTER,
        #[cfg(feature = "bcm2835-sdhci")]
        crate::block::bcm2835::REGISTER,
        #[cfg(feature = "fxmac")]
        crate::net::fxmac::REGISTER,
    ]
}

#[cfg(not(any(windows, unix)))]
pub fn linker_registers() -> &'static [DriverRegister] {
    linker_register_slice()
}

#[cfg(any(windows, unix))]
pub fn linker_registers() -> &'static [DriverRegister] {
    &[]
}

#[cfg(not(any(windows, unix)))]
fn linker_register_slice() -> &'static [DriverRegister] {
    let bytes = linker_register_bytes();
    if bytes.is_empty() {
        return &[];
    }

    unsafe {
        core::slice::from_raw_parts(
            bytes.as_ptr().cast::<DriverRegister>(),
            bytes.len() / core::mem::size_of::<DriverRegister>(),
        )
    }
}

#[cfg(not(any(windows, unix)))]
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
