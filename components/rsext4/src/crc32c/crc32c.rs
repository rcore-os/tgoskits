// crc32c.rs
#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
use crate::crc32c::arm64::*;
use crate::superblock::Ext4Superblock;

const POLY: u32 = 0x82F63B78;

// Build the CRC lookup table at compile time instead of embedding a large literal.
const fn generate_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if (crc & 1) != 0 {
                crc = (crc >> 1) ^ POLY;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC32C_TABLE: [u32; 256] = generate_table();

// Metadata checksum verification is feature-gated by the superblock.
#[inline]
pub fn ext4_superblock_has_metadata_csum(sb: &Ext4Superblock) -> bool {
    sb.has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM)
}

#[inline]
fn crc32c_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc = CRC32C_TABLE[((crc ^ (byte as u32)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}
/// Returns the standard CRC32C initial accumulator.
#[inline]
pub fn crc32c_init() -> u32 {
    0xFFFF_FFFF
}

#[inline]
pub fn crc32c_finalize(crc: u32) -> u32 {
    crc ^ 0xFFFF_FFFF
}

#[inline]
pub fn crc32c_append(crc: u32, data: &[u8]) -> u32 {
    // Use hardware acceleration on aarch64 when the CPU advertises CRC support.
    #[cfg(target_arch = "aarch64")]
    {
        if *HARDWARE_SUPPORT_CRC32 {
            return unsafe { crc32c_hardware(crc, data) };
        }
    }

    crc32c_update(crc, data)
}

#[inline]
pub fn crc32c(data: &[u8]) -> u32 {
    crc32c_finalize(crc32c_append(crc32c_init(), data))
}

/// Returns the CRC32C seed used for ext4 metadata checksums.
///
/// Linux uses the stored checksum seed when `csum_seed` is enabled; otherwise
/// it derives the seed from the filesystem UUID.
pub fn ext4_crc32c_seed_from_superblock(sb: &Ext4Superblock) -> u32 {
    if sb.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_CSUM_SEED) {
        sb.s_checksum_seed
    } else {
        // Do not finalize here; Linux uses the raw running CRC value.
        crc32c_append(crc32c_init(), &sb.s_uuid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_csum_superblock() -> Ext4Superblock {
        let mut sb = Ext4Superblock::default();
        sb.s_feature_ro_compat |= Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM;
        sb.s_uuid = [
            0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE, 0x55, 0xAA, 0x11, 0x22, 0x33, 0x44,
            0x66, 0x88,
        ];
        sb
    }

    #[test]
    fn crc32c_standard_test_vector() {
        // Test idea: keep the canonical Castagnoli vector in place so table generation
        // or hardware fallback changes cannot silently drift from the standard result.
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn crc32c_empty_is_zero() {
        // Test idea: an empty payload should only observe init/finalize and therefore
        // collapse back to zero.
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn crc32c_incremental_equals_one_shot() {
        // Test idea: ext4 appends metadata in pieces, so incremental updates must match
        // the one-shot checksum for the same byte stream.
        let data = b"hello ext4 crc32c";

        let mut crc = crc32c_init();
        crc = crc32c_append(crc, &data[..5]);
        crc = crc32c_append(crc, &data[5..]);
        let inc = crc32c_finalize(crc);

        assert_eq!(inc, crc32c(data));
    }

    #[test]
    fn crc32c_detects_payload_corruption() {
        // Test idea: a single-byte mutation must change the checksum so callers can use
        // the stored CRC to reject damaged metadata.
        let original = b"metadata block";
        let mut corrupted = *original;
        corrupted[3] ^= 0x5A;

        assert_ne!(crc32c(original), crc32c(&corrupted));
    }

    #[test]
    fn seed_uses_uuid_crc_when_csum_seed_feature_is_disabled() {
        // Test idea: without `csum_seed`, ext4 derives the raw seed from the superblock
        // UUID and does not finalize it.
        let sb = metadata_csum_superblock();

        assert_eq!(
            ext4_crc32c_seed_from_superblock(&sb),
            crc32c_append(crc32c_init(), &sb.s_uuid)
        );
    }

    #[test]
    fn seed_uses_stored_checksum_seed_when_feature_is_enabled() {
        // Test idea: once `csum_seed` is enabled, the stored seed must win over UUID-based
        // derivation so every metadata checksum stays stable across mounts.
        let mut sb = metadata_csum_superblock();
        sb.s_feature_incompat |= Ext4Superblock::EXT4_FEATURE_INCOMPAT_CSUM_SEED;
        sb.s_checksum_seed = 0xA1B2_C3D4;

        assert_eq!(ext4_crc32c_seed_from_superblock(&sb), 0xA1B2_C3D4);
    }
}
