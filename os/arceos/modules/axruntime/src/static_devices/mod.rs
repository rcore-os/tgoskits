use rdrive::{Platform, probe::static_::StaticDeviceDesc, register::DriverRegister};

#[cfg(any(feature = "fs", feature = "fs-ng"))]
mod block;
mod dma;
mod pci;
#[cfg(feature = "driver-ramdisk")]
mod ramdisk;
mod virtio;
mod virtio_block;
mod virtio_net;

static STATIC_DEVICES: &[StaticDeviceDesc] = &[
    StaticDeviceDesc::new(virtio::MMIO_DEVICE_NAME),
    StaticDeviceDesc::new(pci::DEVICE_NAME),
    #[cfg(feature = "driver-ramdisk")]
    StaticDeviceDesc::new(ramdisk::DEVICE_NAME),
];

pub(super) fn init() -> Result<(), rdrive::error::DriverError> {
    rdrive::init(Platform::Static(STATIC_DEVICES))?;

    let registers = driver_registers();
    if !registers.is_empty() {
        rdrive::register_append(registers);
    }
    rdrive::register_append(builtin_registers());
    rdrive::probe_pre_kernel().map_err(|err| {
        rdrive::error::DriverError::Unknown(alloc::format!("pre-kernel probe failed: {err:?}"))
    })?;
    rdrive::probe_all(false).map_err(|err| {
        rdrive::error::DriverError::Unknown(alloc::format!("probe failed: {err:?}"))
    })?;
    Ok(())
}

#[cfg(all(feature = "net-ng", not(feature = "plat-dyn")))]
pub(super) fn identity_dma() -> &'static dyn dma_api::DmaOp {
    &dma::IDENTITY_DMA
}

#[cfg(feature = "fs")]
pub(super) fn take_fs_block_devices() -> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs::FsBlockDevice>>
{
    block::take_fs_block_devices()
}

#[cfg(feature = "fs-ng")]
pub(super) fn take_fs_ng_block_devices()
-> alloc::vec::Vec<alloc::boxed::Box<dyn ax_fs_ng::FsBlockDevice>> {
    block::take_fs_ng_block_devices()
}

fn builtin_registers() -> &'static [DriverRegister] {
    &[
        virtio_block::REGISTER,
        virtio_net::REGISTER,
        pci::REGISTER,
        #[cfg(feature = "driver-ramdisk")]
        ramdisk::REGISTER,
    ]
}

fn driver_registers() -> &'static [DriverRegister] {
    let bytes = driver_register_bytes();
    if bytes.is_empty() {
        return &[];
    }
    unsafe {
        core::slice::from_raw_parts(
            bytes.as_ptr() as *const DriverRegister,
            bytes.len() / size_of::<DriverRegister>(),
        )
    }
}

fn driver_register_bytes() -> &'static [u8] {
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
