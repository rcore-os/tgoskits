use core::ptr::NonNull;

use rdrive::probe::OnProbeError;

pub fn iomap(addr: usize, size: usize) -> Result<NonNull<u8>, OnProbeError> {
    axklib::mmio::ioremap_raw(addr.into(), size)
        .map_err(|err| OnProbeError::Other(alloc::format!("{err:?}").into()))
        .map(|mmio| mmio.as_nonnull_ptr())
}
