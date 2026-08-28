//! Linux-compatible ext4 directory hashing.

use super::HashTreeError;
use crate::entries::Ext4DxRootInfo;

const DEFAULT_SEED: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
const TEA_DELTA: u32 = 0x9e37_79b9;
const HALF_MD4_K2: u32 = 0x5a82_7999;
const HALF_MD4_K3: u32 = 0x6ed9_eba1;

/// The major and minor hashes stored by ext4 HTree indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryHash {
    /// Major hash used to select an HTree entry.
    pub major: u32,
    /// Secondary hash used by formats that preserve a 64-bit result.
    pub minor: u32,
}

/// Calculates the ext4 directory hash selected by an on-disk HTree root.
pub fn calculate_hash(
    name: &[u8],
    hash_version: u8,
    hash_seed: &[u32; 4],
) -> Result<DirectoryHash, HashTreeError> {
    let mut state = if hash_seed.iter().any(|word| *word != 0) {
        *hash_seed
    } else {
        DEFAULT_SEED
    };

    let (major, minor) = match hash_version {
        Ext4DxRootInfo::DX_HASH_LEGACY => (legacy_hash(name, true), 0),
        Ext4DxRootInfo::DX_HASH_LEGACY_UNSIGNED => (legacy_hash(name, false), 0),
        Ext4DxRootInfo::DX_HASH_HALF_MD4 | Ext4DxRootInfo::DX_HASH_HALF_MD4_UNSIGNED => {
            let signed = hash_version == Ext4DxRootInfo::DX_HASH_HALF_MD4;
            for chunk_start in (0..name.len()).step_by(32) {
                let remaining = &name[chunk_start..];
                let input = hash_buffer::<8>(remaining, signed);
                half_md4_transform(&mut state, &input);
            }
            (state[1], state[2])
        }
        Ext4DxRootInfo::DX_HASH_TEA | Ext4DxRootInfo::DX_HASH_TEA_UNSIGNED => {
            let signed = hash_version == Ext4DxRootInfo::DX_HASH_TEA;
            for chunk_start in (0..name.len()).step_by(16) {
                let remaining = &name[chunk_start..];
                let input = hash_buffer::<4>(remaining, signed);
                tea_transform(&mut state, &input);
            }
            (state[0], state[1])
        }
        _ => return Err(HashTreeError::UnsupportedHashVersion),
    };

    let mut major = major & !1;
    if major == 0xffff_fffe {
        major = 0xffff_fffc;
    }

    Ok(DirectoryHash { major, minor })
}

fn legacy_hash(name: &[u8], signed: bool) -> u32 {
    let mut hash0 = 0x12a3_fe2d_u32;
    let mut hash1 = 0x37ab_e8f9_u32;

    for byte in name {
        let value = if signed {
            (*byte as i8 as i32).wrapping_mul(7_152_373) as u32
        } else {
            u32::from(*byte).wrapping_mul(7_152_373)
        };
        let mut hash = hash1.wrapping_add(hash0 ^ value);
        if hash & 0x8000_0000 != 0 {
            hash = hash.wrapping_sub(0x7fff_ffff);
        }
        hash1 = hash0;
        hash0 = hash;
    }

    hash0.wrapping_shl(1)
}

fn hash_buffer<const WORDS: usize>(remaining: &[u8], signed: bool) -> [u32; WORDS] {
    let remaining_len = remaining.len() as u32;
    let mut pad = remaining_len | remaining_len.wrapping_shl(8);
    pad |= pad.wrapping_shl(16);

    let mut output = [pad; WORDS];
    let mut value = pad;
    let byte_count = remaining.len().min(WORDS * 4);
    for (index, byte) in remaining[..byte_count].iter().enumerate() {
        let byte_value = if signed {
            *byte as i8 as i32 as u32
        } else {
            u32::from(*byte)
        };
        value = byte_value.wrapping_add(value.wrapping_shl(8));
        if index % 4 == 3 {
            output[index / 4] = value;
            value = pad;
        }
    }
    if !byte_count.is_multiple_of(4) {
        output[byte_count / 4] = value;
    }

    output
}

fn tea_transform(state: &mut [u32; 4], input: &[u32; 4]) {
    let mut sum = 0_u32;
    let mut first = state[0];
    let mut second = state[1];

    for _ in 0..16 {
        sum = sum.wrapping_add(TEA_DELTA);
        first = first.wrapping_add(
            (second.wrapping_shl(4).wrapping_add(input[0]))
                ^ second.wrapping_add(sum)
                ^ (second.wrapping_shr(5).wrapping_add(input[1])),
        );
        second = second.wrapping_add(
            (first.wrapping_shl(4).wrapping_add(input[2]))
                ^ first.wrapping_add(sum)
                ^ (first.wrapping_shr(5).wrapping_add(input[3])),
        );
    }

    state[0] = state[0].wrapping_add(first);
    state[1] = state[1].wrapping_add(second);
}

fn half_md4_transform(state: &mut [u32; 4], input: &[u32; 8]) {
    macro_rules! round {
        (
            $function:expr,
            $target:ident,
            $first:ident,
            $second:ident,
            $third:ident,
            $word:expr,
            $shift:expr
        ) => {
            $target = $target
                .wrapping_add($function($first, $second, $third))
                .wrapping_add($word)
                .rotate_left($shift);
        };
    }

    let f = |x: u32, y: u32, z: u32| z ^ (x & (y ^ z));
    let g = |x: u32, y: u32, z: u32| (x & y).wrapping_add((x ^ y) & z);
    let h = |x: u32, y: u32, z: u32| x ^ y ^ z;
    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];

    round!(f, a, b, c, d, input[0], 3);
    round!(f, d, a, b, c, input[1], 7);
    round!(f, c, d, a, b, input[2], 11);
    round!(f, b, c, d, a, input[3], 19);
    round!(f, a, b, c, d, input[4], 3);
    round!(f, d, a, b, c, input[5], 7);
    round!(f, c, d, a, b, input[6], 11);
    round!(f, b, c, d, a, input[7], 19);

    round!(g, a, b, c, d, input[1].wrapping_add(HALF_MD4_K2), 3);
    round!(g, d, a, b, c, input[3].wrapping_add(HALF_MD4_K2), 5);
    round!(g, c, d, a, b, input[5].wrapping_add(HALF_MD4_K2), 9);
    round!(g, b, c, d, a, input[7].wrapping_add(HALF_MD4_K2), 13);
    round!(g, a, b, c, d, input[0].wrapping_add(HALF_MD4_K2), 3);
    round!(g, d, a, b, c, input[2].wrapping_add(HALF_MD4_K2), 5);
    round!(g, c, d, a, b, input[4].wrapping_add(HALF_MD4_K2), 9);
    round!(g, b, c, d, a, input[6].wrapping_add(HALF_MD4_K2), 13);

    round!(h, a, b, c, d, input[3].wrapping_add(HALF_MD4_K3), 3);
    round!(h, d, a, b, c, input[7].wrapping_add(HALF_MD4_K3), 9);
    round!(h, c, d, a, b, input[2].wrapping_add(HALF_MD4_K3), 11);
    round!(h, b, c, d, a, input[6].wrapping_add(HALF_MD4_K3), 15);
    round!(h, a, b, c, d, input[1].wrapping_add(HALF_MD4_K3), 3);
    round!(h, d, a, b, c, input[5].wrapping_add(HALF_MD4_K3), 9);
    round!(h, c, d, a, b, input[0].wrapping_add(HALF_MD4_K3), 11);
    round!(h, b, c, d, a, input[4].wrapping_add(HALF_MD4_K3), 15);

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}
