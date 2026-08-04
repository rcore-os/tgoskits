/// Extent tree header.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentHeader {
    pub eh_magic: u16,
    pub eh_entries: u16,
    pub eh_max: u16,
    pub eh_depth: u16,
    pub eh_generation: u32,
}

impl Default for Ext4ExtentHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl Ext4ExtentHeader {
    pub const EXT4_EXT_MAGIC: u16 = 0xF30A;

    pub fn new() -> Self {
        Self {
            eh_magic: Self::EXT4_EXT_MAGIC,
            eh_entries: 0,
            eh_max: 4,
            eh_depth: 0,
            eh_generation: 0,
        }
    }
}

/// Extent tree index entry.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4ExtentIdx {
    pub ei_block: u32,
    pub ei_leaf_lo: u32,
    pub ei_leaf_hi: u16,
    pub ei_unused: u16,
}

/// Extent tree leaf entry.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4Extent {
    pub ee_block: u32,
    pub ee_len: u16,
    pub ee_start_hi: u16,
    pub ee_start_lo: u32,
}

impl Default for Ext4Extent {
    fn default() -> Self {
        Self {
            ee_block: 0,
            ee_len: Self::EXT_INIT_MAX_LEN,
            ee_start_hi: 0,
            ee_start_lo: 0,
        }
    }
}

impl Ext4Extent {
    pub const EXT_INIT_MAX_LEN: u16 = 32768;
    pub const EXT_UNINIT_MAX_LEN: u16 = 32767;
    pub const EXT_UNWRITTEN_FLAG: u16 = 0x8000;

    pub fn new(logic_start: u32, start_phy_block: u64, len: u16) -> Self {
        let high = (start_phy_block >> 32) as u16;
        let low = (start_phy_block & 0xffff_ffff) as u32;
        Self {
            ee_block: logic_start,
            ee_len: len,
            ee_start_hi: high,
            ee_start_lo: low,
        }
    }

    pub fn start_block(&self) -> u64 {
        (self.ee_start_hi as u64) << 32 | self.ee_start_lo as u64
    }

    /// Returns the logical length encoded by ext4's initialized/unwritten extent format.
    pub fn len(&self) -> u32 {
        Self::decode_len(self.ee_len)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_unwritten(&self) -> bool {
        self.ee_len > Self::EXT_UNWRITTEN_FLAG
    }

    pub fn is_initialized(&self) -> bool {
        !self.is_unwritten()
    }

    pub fn encode_len(len: u32, unwritten: bool) -> Option<u16> {
        if len == 0 {
            return None;
        }

        if unwritten {
            if len > Self::EXT_UNINIT_MAX_LEN as u32 {
                return None;
            }
            Some((len as u16) | Self::EXT_UNWRITTEN_FLAG)
        } else {
            if len > Self::EXT_INIT_MAX_LEN as u32 {
                return None;
            }
            Some(if len == Self::EXT_INIT_MAX_LEN as u32 {
                Self::EXT_INIT_MAX_LEN
            } else {
                len as u16
            })
        }
    }

    pub fn decode_len(raw_len: u16) -> u32 {
        if raw_len == Self::EXT_INIT_MAX_LEN {
            Self::EXT_INIT_MAX_LEN as u32
        } else {
            (raw_len & !Self::EXT_UNWRITTEN_FLAG) as u32
        }
    }

    pub fn build_len_like(&self, len: u32) -> Option<u16> {
        Self::encode_len(len, self.is_unwritten())
    }
}
