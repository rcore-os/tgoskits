/// 字节序转换辅助模块
/// 
/// Ext4 磁盘格式使用小端序（Little Endian）
/// 本模块提供在内存表示和磁盘表示之间转换的辅助函数

use core::mem::size_of;

/// 从小端字节序读取 u16
#[inline]
pub fn read_u16_le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// 从小端字节序读取 u32
#[inline]
pub fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// 从小端字节序读取 u64
#[inline]
pub fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

/// 写入 u16 为小端字节序
#[inline]
pub fn write_u16_le(value: u16, bytes: &mut [u8]) {
    let le_bytes = value.to_le_bytes();
    bytes[0] = le_bytes[0];
    bytes[1] = le_bytes[1];
}

/// 写入 u32 为小端字节序
#[inline]
pub fn write_u32_le(value: u32, bytes: &mut [u8]) {
    let le_bytes = value.to_le_bytes();
    bytes[0..4].copy_from_slice(&le_bytes);
}

/// 写入 u64 为小端字节序
#[inline]
pub fn write_u64_le(value: u64, bytes: &mut [u8]) {
    let le_bytes = value.to_le_bytes();
    bytes[0..8].copy_from_slice(&le_bytes);
}

/// 可以从字节序列化/反序列化的 trait
pub trait DiskFormat: Sized {
    /// 从磁盘字节（小端序）反序列化
    fn from_disk_bytes(bytes: &[u8]) -> Self;
    
    /// 序列化到磁盘字节（小端序）
    fn to_disk_bytes(&self, bytes: &mut [u8]);
    
    /// 磁盘大小（字节）
    fn disk_size() -> usize {
        size_of::<Self>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u16_conversion() {
        let value = 0x1234u16;
        let mut bytes = [0u8; 2];
        
        write_u16_le(value, &mut bytes);
        assert_eq!(bytes, [0x34, 0x12]); // 小端序：低字节在前
        
        let read_value = read_u16_le(&bytes);
        assert_eq!(read_value, value);
    }

    #[test]
    fn test_u32_conversion() {
        let value = 0x12345678u32;
        let mut bytes = [0u8; 4];
        
        write_u32_le(value, &mut bytes);
        assert_eq!(bytes, [0x78, 0x56, 0x34, 0x12]); // 小端序
        
        let read_value = read_u32_le(&bytes);
        assert_eq!(read_value, value);
    }

    #[test]
    fn test_u64_conversion() {
        let value = 0x123456789ABCDEF0u64;
        let mut bytes = [0u8; 8];
        
        write_u64_le(value, &mut bytes);
        assert_eq!(bytes, [0xF0, 0xDE, 0xBC, 0x9A, 0x78, 0x56, 0x34, 0x12]);
        
        let read_value = read_u64_le(&bytes);
        assert_eq!(read_value, value);
    }
}
