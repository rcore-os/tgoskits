//! Internal helpers shared by native SD/MMC protocol paths.

/// Convert a logical 512-byte block address to the address argument that
/// CMD17/CMD18/CMD24/CMD25 expect on the wire.
///
/// SDHC/SDXC cards use block addressing directly; SDSC cards expect byte
/// addresses, so the block index is multiplied by 512.
#[cfg(feature = "sdio")]
#[inline]
pub(crate) fn block_addr_of(addr: u32, high_capacity: bool) -> u32 {
    if high_capacity { addr } else { addr * 512 }
}
