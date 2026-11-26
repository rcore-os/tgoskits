//! Kani harnesses for the directory entry parse and the directory block walk: each is total, the
//! walk ends, and every name it returns lies inside its own record. Behind `#[cfg(kani)]`, so only
//! `cargo kani` compiles them.

use super::{DirEntryIterator, Ext4DirEntryInfo};

/// **`Ext4DirEntryInfo::parse_from_bytes` is total on every slice of up to 24 bytes**, and the
/// name it returns lies inside the slice, after the eight-byte entry header.
#[kani::proof]
#[kani::unwind(2)]
fn directory_entry_parse_is_total() {
    let storage: [u8; 24] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= storage.len());
    let data = &storage[..length];
    if let Some(entry) = Ext4DirEntryInfo::parse_from_bytes(data) {
        let start = data.as_ptr() as usize;
        let name = entry.name.as_ptr() as usize;
        assert!(name == start + 8);
        assert!(name + entry.name.len() <= start + data.len());
        assert!(entry.inode != 0);
    }
}

/// **Walking a directory block of up to 24 bytes ends**, after at most one step per eight bytes,
/// and every entry it yields, name included, lies inside the record it was read from, and that
/// record inside the block.
#[kani::proof]
#[kani::unwind(5)]
#[kani::solver(cadical)]
fn directory_block_walk_ends_inside_its_block() {
    let storage: [u8; 24] = kani::any();
    let length: usize = kani::any();
    kani::assume(length <= storage.len());
    let block = &storage[..length];
    let start = block.as_ptr() as usize;
    let mut steps = 0usize;
    let mut previous_offset = None;
    for (entry, offset) in DirEntryIterator::new(block) {
        steps += 1;
        assert!(steps <= block.len() / 8);
        assert!(previous_offset.is_none_or(|previous| offset >= previous + 8));
        previous_offset = Some(offset);
        let rec_len = usize::from(u16::from_le_bytes([block[offset + 4], block[offset + 5]]));
        assert!(offset + rec_len <= block.len());
        let name = entry.name.as_ptr() as usize;
        assert!(name == start + offset + 8);
        assert!(name + entry.name.len() <= start + offset + rec_len);
    }
}
