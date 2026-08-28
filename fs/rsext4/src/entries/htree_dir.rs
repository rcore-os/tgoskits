//! Private HTree constants and decoded index entries.

/// Namespace for HTree root wire constants.
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxRootInfo;

impl Ext4DxRootInfo {
    pub const INFO_LENGTH: u8 = 8;
}

impl Ext4DxRootInfo {
    pub const DX_HASH_LEGACY: u8 = 0;
    pub const DX_HASH_HALF_MD4: u8 = 1;
    pub const DX_HASH_TEA: u8 = 2;
    pub const DX_HASH_LEGACY_UNSIGNED: u8 = 3;
    pub const DX_HASH_HALF_MD4_UNSIGNED: u8 = 4;
    pub const DX_HASH_TEA_UNSIGNED: u8 = 5;
    pub const DX_HASH_SIPHASH: u8 = 6;
}

/// HTree index entry.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxEntry {
    pub hash: u32,
    pub block: u32,
}
