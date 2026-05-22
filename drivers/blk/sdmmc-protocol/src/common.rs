//! Internal helpers shared between the SPI and SDIO drivers.

/// Convert a logical 512-byte block address to the address argument that
/// CMD17/CMD18/CMD24/CMD25 expect on the wire.
///
/// SDHC/SDXC cards use block addressing directly; SDSC cards expect byte
/// addresses, so the block index is multiplied by 512.
#[cfg(any(feature = "spi", feature = "sdio"))]
#[inline]
pub(crate) fn block_addr_of(addr: u32, high_capacity: bool) -> u32 {
    if high_capacity { addr } else { addr * 512 }
}

/// CRC-16/CCITT (poly 0x1021, init 0x0000, no reflection, no final xor).
///
/// SD spec section 4.5 specifies this CRC for data blocks. SPI mode allows
/// the CRC to be ignored on receive but the bytes still need to be sent on
/// transmit, so callers should *generate* it and *optionally* verify it.
#[cfg(feature = "spi")]
pub(crate) fn crc16_ccitt(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in bytes {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "spi")]
    use super::crc16_ccitt;

    #[cfg(feature = "spi")]
    #[test]
    fn crc16_ccitt_known_vectors() {
        // CRC-16/XMODEM (SD spec data CRC) check value for ASCII "123456789".
        assert_eq!(crc16_ccitt(b"123456789"), 0x31C3);
        assert_eq!(crc16_ccitt(&[]), 0);
        // 512 zero bytes → CRC is 0 (the polynomial accumulator stays at 0
        // when feeding zeros into an init-0 register).
        assert_eq!(crc16_ccitt(&[0u8; 512]), 0);
    }
}
