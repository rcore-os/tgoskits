use crate::constants::*;

/// VirtIO Block Device Configuration
/// Layout according to VirtIO specification
#[derive(Debug, Clone)]
pub struct VirtioBlockConfig {
    /// Total capacity in 512-byte sectors (8 bytes at offset 0x00)
    pub capacity: u64,
    /// Maximum segment size (4 bytes at offset 0x08)
    pub size_max: u32,
    /// Maximum number of segments (4 bytes at offset 0x0c)
    pub seg_max: u32,
    /// Geometry cylinders (2 bytes at offset 0x10)
    pub cylinders: u16,
    /// Geometry heads (1 byte at offset 0x12)
    pub heads: u8,
    /// Geometry sectors (1 byte at offset 0x13)
    pub sectors: u8,
    /// Block size in bytes (4 bytes at offset 0x14)
    pub blk_size: u32,
    /// Physical block exponent (1 byte at offset 0x18)
    pub physical_block_exp: u8,
    /// Alignment offset (1 byte at offset 0x19)
    pub alignment_offset: u8,
    /// Minimum I/O size (2 bytes at offset 0x1a)
    pub min_io_size: u16,
    /// Optimal I/O size (4 bytes at offset 0x1c)
    pub opt_io_size: u32,
}

impl Default for VirtioBlockConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_CAPACITY_SECTORS,
            size_max: DEFAULT_SIZE_MAX,
            seg_max: DEFAULT_SEG_MAX,
            cylinders: DEFAULT_CYLINDERS,
            heads: DEFAULT_HEADS,
            sectors: DEFAULT_SECTORS,
            blk_size: SECTOR_SIZE,
            physical_block_exp: DEFAULT_PHYSICAL_BLOCK_EXP,
            alignment_offset: DEFAULT_ALIGNMENT_OFFSET,
            min_io_size: DEFAULT_MIN_IO_SIZE,
            opt_io_size: DEFAULT_OPT_IO_SIZE,
        }
    }
}
