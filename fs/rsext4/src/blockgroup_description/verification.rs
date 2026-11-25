//! Kani harness for the group descriptor decode: it is total, accepts exactly the record sizes
//! ext4 defines, and reads no field of a short record from past its end. Behind `#[cfg(kani)]`,
//! so only `cargo kani` compiles it.

use super::Ext4GroupDesc;

/// **`Ext4GroupDesc::decode_checked` is total on every slice of up to 128 bytes**: it never
/// panics, accepts exactly the 32-byte record and the power-of-two records from 64 bytes, and
/// reads no field of a 32-byte record from past its end (every high half decodes as zero).
#[kani::proof]
#[kani::unwind(2)]
fn group_descriptor_decode_is_total_and_stays_in_its_record() {
    let storage: [u8; 128] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= storage.len());
    let bytes = &storage[..length];
    let accepted = bytes.len() == 32 || bytes.len() == 64 || bytes.len() == 128;
    match Ext4GroupDesc::decode_checked(bytes) {
        Ok(desc) => {
            assert!(accepted);
            if bytes.len() == 32 {
                assert_eq!(desc.bg_block_bitmap_hi, 0);
                assert_eq!(desc.bg_inode_bitmap_hi, 0);
                assert_eq!(desc.bg_inode_table_hi, 0);
                assert_eq!(desc.bg_free_blocks_count_hi, 0);
                assert_eq!(desc.bg_free_inodes_count_hi, 0);
                assert_eq!(desc.bg_used_dirs_count_hi, 0);
                assert_eq!(desc.bg_itable_unused_hi, 0);
            }
        }
        Err(_) => assert!(!accepted),
    }
}
