use core::fmt;

use bitfield_struct::bitfield;

/// Card Identification
///
/// Reference: https://www.cameramemoryspeed.com/sd-memory-card-faq/reading-sd-card-cid-serial-psn-internal-numbers/
#[bitfield(u128, order = Msb, debug = false)]
pub struct Cid {
    /// Manufacturer ID
    pub mid: u8,
    /// OEM/Application ID
    pub oid: u16,
    /// Product name
    #[bits(40)]
    pub pnm: u64,
    /// Product Revision
    #[bits(8)]
    pub prv: ProductRevision,
    /// Product Serial Number
    pub psn: u32,
    /// Manufacturing Date
    #[bits(16)]
    pub mdt: ManufacturingDate,
    /// CRC7 checksum
    #[bits(7)]
    pub crc: u8,
    __: bool,
}

#[bitfield(u8, order = Msb, debug = false)]
pub struct ProductRevision {
    #[bits(4)]
    pub hwrev: u8,
    #[bits(4)]
    pub fwrev: u8,
}

impl fmt::Debug for ProductRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.hwrev(), self.fwrev())
    }
}

#[bitfield(u16, order = Msb, debug = false)]
pub struct ManufacturingDate {
    #[bits(4)]
    __: u8,
    /// Manufacture Date Code - Year
    pub year: u8,
    /// Manufacture Date Code - Month
    #[bits(4)]
    pub month: u8,
}

impl fmt::Debug for ManufacturingDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}/{}", self.month(), self.year() as u32 + 2000)
    }
}

impl fmt::Debug for Cid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cid")
            .field("mid", &self.mid())
            .field(
                "oid",
                &str::from_utf8(&self.oid().to_be_bytes()).unwrap_or("Invalid OEM ID"),
            )
            .field(
                "pnm",
                &str::from_utf8(&self.pnm().to_be_bytes()[3..8]).unwrap_or("Invalid Product Name"),
            )
            .field("prv", &self.prv())
            .field("psn", &self.psn())
            .field("mdt", &self.mdt())
            .field("crc", &self.crc())
            .finish()
    }
}

/// Card Specific Data, version 2
#[bitfield(u128, order = Msb)]
pub struct CsdV2 {
    /// CSD structure
    #[bits(2)]
    pub csd_structure: u8,
    #[bits(6)]
    __: u8,
    /// Data read access time 1
    pub taac: u8,
    /// Data write access time 1
    pub nsac: u8,
    /// Max data transfer rate
    pub tran_speed: u8,
    /// Card command class
    #[bits(12)]
    pub ccc: u16,
    /// Max read block length
    #[bits(4)]
    pub read_bl_len: u8,
    /// Partial blocks for read allowed
    pub read_blk_partial: bool,
    /// Write block misalignment
    pub write_blk_misaligned: bool,
    /// Read block misalignment
    pub read_blk_misaligned: bool,
    /// DSR implemented
    pub dsr_imp: bool,
    #[bits(6)]
    __: u8,
    /// Device size
    #[bits(22)]
    pub c_size: u32,
    __: bool,
    /// Erase single block enabled
    pub erase_blk_en: bool,
    /// Erase sector size
    #[bits(7)]
    pub sector_size: u8,
    /// Write protect group size
    #[bits(7)]
    pub wp_grp_size: u8,
    /// Write protect group enable
    pub wp_grp_enable: bool,
    #[bits(2)]
    __: u8,
    /// Write speed factor
    #[bits(3)]
    pub r2w_factor: u8,
    /// Max write block length
    #[bits(4)]
    pub write_bl_len: u8,
    /// Partial blocks for write allowed
    pub write_blk_partial: bool,
    #[bits(5)]
    __: u8,
    /// File format group
    pub file_format_grp: bool,
    /// Copy flag
    pub copy: bool,
    /// Permanent write protection
    pub perm_write_protect: bool,
    /// Temporary write protection
    pub tmp_write_protect: bool,
    /// File format
    #[bits(2)]
    pub file_format: u8,
    #[bits(2)]
    __: u8,
    /// CRC checksum
    #[bits(7)]
    pub crc: u8,
    __: bool,
}

impl CsdV2 {
    /// Returns the number of blocks.
    pub fn num_blocks(&self) -> u64 {
        (self.c_size() as u64 + 1) * 1024
    }
}
