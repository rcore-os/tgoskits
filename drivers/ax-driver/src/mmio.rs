use core::ptr::NonNull;

use rdrive::probe::OnProbeError;

pub fn iomap(addr: usize, size: usize) -> Result<NonNull<u8>, OnProbeError> {
    axklib::mmio::ioremap_raw(addr.into(), size)
        .map_err(|err| OnProbeError::Other(alloc::format!("{err:?}").into()))
        .map(|mmio| mmio.as_nonnull_ptr())
}

pub(crate) fn iomap_firmware_device(
    device_name: &str,
    addr: usize,
    size: usize,
) -> Result<NonNull<u8>, OnProbeError> {
    if size == 0 {
        return Err(OnProbeError::other(alloc::format!(
            "{device_name} MMIO region has zero size"
        )));
    }

    let paddr = firmware_addr_to_phys(addr);
    iomap(paddr, size)
}

pub(crate) fn firmware_addr_to_phys(addr: usize) -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        const LOONGARCH_PADDR_MASK: usize = (1usize << 48) - 1;
        addr & LOONGARCH_PADDR_MASK
    }

    #[cfg(not(target_arch = "loongarch64"))]
    {
        addr
    }
}

#[cfg(target_arch = "loongarch64")]
pub(crate) fn loongarch_uncached_addr(addr: usize) -> usize {
    // Used for DMA aliases that must bypass cache. Device MMIO should go
    // through iomap(), whose LoongArch backend already returns uncached DMW.
    const LOONGARCH_UNCACHED_DMW_BASE: usize = 0x8000_0000_0000_0000;
    LOONGARCH_UNCACHED_DMW_BASE | firmware_addr_to_phys(addr)
}
