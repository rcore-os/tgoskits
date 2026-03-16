// crc32c.rs
#[cfg(target_arch = "aarch64")]
#[allow(dead_code)]
use crate::ext4_backend::crc32c::arm64::*;
use crate::ext4_backend::superblock::Ext4Superblock;

const POLY: u32 = 0x82F63B78;

// 编译时生成 CRC 表，无需硬编码巨大的数组
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
                crc = crc >> 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC32C_TABLE: [u32; 256] = generate_table();

// 超级块是否开启 metadata_csum 特性，决定是否启用 checksum 验证
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
/// 初始化 CRC32C 校验和计算
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
    // aarch64 架构的crc32c硬件加速
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

/// 获取 ext4 文件系统的 CRC32C 种子值.用于某些元数据结构的校验和计算.
///
/// 内核逻辑：
///   if (ext4_has_feature_csum_seed(sb))
///       sbi->s_csum_seed = le32_to_cpu(es->s_checksum_seed);
///   else if (ext4_has_metadata_csum(sb))
///       sbi->s_csum_seed = ext4_chksum(~0, es->s_uuid, 16);
pub fn ext4_crc32c_seed_from_superblock(sb: &Ext4Superblock) -> u32 {
    if sb.has_feature_incompat(Ext4Superblock::EXT4_FEATURE_INCOMPAT_CSUM_SEED) {
        sb.s_checksum_seed
    } else {
        // 不 finalize，与内核 __crc32c_le(~0, uuid, 16) 对齐
        crc32c_append(crc32c_init(), &sb.s_uuid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_standard_test_vector() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    #[test]
    fn crc32c_empty_is_zero() {
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn crc32c_incremental_equals_one_shot() {
        let data = b"hello ext4 crc32c";

        let mut crc = crc32c_init();
        crc = crc32c_append(crc, &data[..5]);
        crc = crc32c_append(crc, &data[5..]);
        let inc = crc32c_finalize(crc);

        assert_eq!(inc, crc32c(data));
    }
}
