//! HTree directory structures and hashing helpers.

use super::Ext4DirEntry2;

/// Root block layout for an HTree directory.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxRoot {
    pub dot: Ext4DirEntry2,
    pub dotdot: Ext4DirEntry2,
    pub info: Ext4DxRootInfo,
}

/// HTree root metadata.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxRootInfo {
    pub reserved_zero: u32,
    pub hash_version: u8,
    pub info_length: u8,
    pub indirect_levels: u8,
    pub unused_flags: u8,
}

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
}

/// HTree index entry.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxEntry {
    pub hash: u32,
    pub block: u32,
}

/// Entry count and limit header used by HTree blocks.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext4DxCountlimit {
    pub limit: u16,
    pub count: u16,
}

/// Full HTree node header.
#[repr(C)]
#[derive(Debug)]
pub struct Ext4DxNode {
    pub fake: Ext4DirEntry2,
    pub countlimit: Ext4DxCountlimit,
}

/// Calculates a directory hash for an HTree directory.
pub fn calculate_hash(name: &[u8], hash_version: u8, hash_seed: &[u32; 4]) -> u32 {
    match hash_version {
        Ext4DxRootInfo::DX_HASH_LEGACY => legacy_hash(name),
        Ext4DxRootInfo::DX_HASH_HALF_MD4 => half_md4_hash(name, hash_seed),
        Ext4DxRootInfo::DX_HASH_TEA => tea_hash(name, hash_seed),
        _ => 0,
    }
}

fn legacy_hash(name: &[u8]) -> u32 {
    let mut hash = 0u32;
    for &byte in name {
        hash = hash.wrapping_mul(33).wrapping_add(byte as u32);
    }
    hash
}

fn half_md4_hash(name: &[u8], seed: &[u32; 4]) -> u32 {
    let mut hash = seed[0];
    for &byte in name {
        hash = hash.wrapping_mul(1103515245).wrapping_add(byte as u32);
    }
    hash
}

fn tea_hash(name: &[u8], seed: &[u32; 4]) -> u32 {
    let mut hash = seed[0];
    let mut buf = [0u32; 4];

    for chunk in name.chunks(16) {
        for (i, bytes) in chunk.chunks(4).enumerate() {
            if i >= 4 {
                break;
            }
            let mut val = 0u32;
            for &b in bytes {
                val = (val << 8) | b as u32;
            }
            buf[i] = val;
        }

        for _ in 0..4 {
            hash = hash.wrapping_add(buf[0] ^ buf[1]);
        }
    }
    hash
}
