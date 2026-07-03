use core::ptr::NonNull;

use rdrive::probe::OnProbeError;

pub fn iomap(addr: usize, size: usize) -> Result<NonNull<u8>, OnProbeError> {
    axklib::mmio::ioremap_raw(addr.into(), size)
        .map_err(|err| OnProbeError::Other(alloc::format!("{err:?}").into()))
        .map(|mmio| mmio.as_nonnull_ptr())
}

// Firmware tables can carry CPU-visible aliases, for example LoongArch DMW
// addresses. Normalize those addresses before crossing into the physical MMIO
// mapping backend.
#[cfg(any(
    feature = "ls2k1000-ahci",
    feature = "ls2k1000-gmac",
    all(feature = "rtc", target_arch = "loongarch64"),
))]
pub(crate) fn iomap_firmware_reg(
    device_name: &str,
    addr: u64,
    size: Option<u64>,
    default_size: usize,
) -> Result<NonNull<u8>, OnProbeError> {
    iomap_firmware_device(
        device_name,
        addr as usize,
        firmware_reg_size(size, default_size),
    )
}

#[cfg(any(
    feature = "serial",
    feature = "ls2k1000-ahci",
    feature = "ls2k1000-gmac",
    all(feature = "rtc", target_arch = "loongarch64"),
))]
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

#[cfg(any(
    feature = "ls2k1000-ahci",
    feature = "ls2k1000-gmac",
    all(feature = "rtc", target_arch = "loongarch64"),
))]
pub(crate) fn firmware_reg_paddr(addr: u64) -> usize {
    firmware_addr_to_phys(addr as usize)
}

#[cfg(any(
    feature = "ls2k1000-ahci",
    feature = "ls2k1000-gmac",
    all(feature = "rtc", target_arch = "loongarch64"),
))]
pub(crate) fn firmware_reg_size(size: Option<u64>, default_size: usize) -> usize {
    size.unwrap_or(default_size as u64) as usize
}

#[cfg(any(
    feature = "serial",
    feature = "ls2k1000-ahci",
    feature = "ls2k1000-gmac",
    all(feature = "rtc", target_arch = "loongarch64"),
))]
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

#[cfg(all(target_arch = "loongarch64", feature = "ls2k1000-gmac"))]
pub(crate) fn loongarch_uncached_addr(addr: usize) -> usize {
    // Used for DMA aliases that must bypass cache. Device MMIO should go
    // through iomap(), whose LoongArch backend already returns uncached DMW.
    const LOONGARCH_UNCACHED_DMW_BASE: usize = 0x8000_0000_0000_0000;
    LOONGARCH_UNCACHED_DMW_BASE | firmware_addr_to_phys(addr)
}
