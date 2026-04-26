use alloc::{vec, vec::Vec};
use core::cell::Cell;

use super::*;
use crate::{
    blockdev::{BlockDevice, Jbd2Dev},
    bmalloc::{AbsoluteBN, BlockAllocator, InodeAllocator, InodeNumber},
    config::DEFAULT_INODE_SIZE,
    disknode::{Ext4Inode, Ext4Timestamp},
    error::Ext4Error,
    ext4::Ext4FileSystem,
};

struct MockBlockDevice {
    data: Vec<u8>,
    is_open: bool,
    now: Cell<i64>,
}

impl MockBlockDevice {
    fn new(size: usize) -> Self {
        let data = vec![0; size];
        Self {
            data,
            is_open: false,
            now: Cell::new(1_700_000_000),
        }
    }
}

impl BlockDevice for MockBlockDevice {
    fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Result<(), Ext4Error> {
        if !self.is_open {
            return Err(Ext4Error::badf());
        }

        let start = block_id.as_usize()? * 512;
        let end = start + (count as usize) * 512;
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / 512) as u64,
            ));
        }

        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn read(
        &mut self,
        buffer: &mut [u8],
        block_id: AbsoluteBN,
        count: u32,
    ) -> Result<(), Ext4Error> {
        if !self.is_open {
            return Err(Ext4Error::badf());
        }

        let start = block_id.as_usize()? * 512;
        let end = start + (count as usize) * 512;
        if end > self.data.len() {
            return Err(Ext4Error::block_out_of_range(
                block_id.to_u32()?,
                (self.data.len() / 512) as u64,
            ));
        }

        buffer.copy_from_slice(&self.data[start..end]);
        Ok(())
    }

    fn open(&mut self) -> Result<(), Ext4Error> {
        self.is_open = true;
        Ok(())
    }

    fn close(&mut self) -> Result<(), Ext4Error> {
        self.is_open = false;
        Ok(())
    }

    fn total_blocks(&self) -> u64 {
        (self.data.len() / 512) as u64
    }

    fn current_time(&self) -> Result<Ext4Timestamp, Ext4Error> {
        let sec = self.now.get();
        self.now.set(sec + 1);
        Ok(Ext4Timestamp::new(sec, 0))
    }
}

fn create_test_fs() -> Ext4FileSystem {
    use crate::{
        cache::{BitmapCache, DataBlockCache, InodeCache},
        superblock::Ext4Superblock,
    };

    let superblock = Ext4Superblock {
        s_hash_seed: [0x12345678, 0x87654321, 0xABCDEF00, 0x00FEDCBA],
        s_def_hash_version: 0x8,
        ..Default::default()
    };

    let inode_size = match superblock.s_inode_size {
        0 => DEFAULT_INODE_SIZE as usize,
        n => n as usize,
    };

    Ext4FileSystem {
        superblock,
        group_descs: Vec::new(),
        block_allocator: BlockAllocator::new(&superblock),
        inode_allocator: InodeAllocator::new(&superblock),
        bitmap_cache: BitmapCache::new(100),
        inodetable_cahce: InodeCache::new(100, inode_size),
        datablock_cache: DataBlockCache::new(100, 4096),
        root_inode: InodeNumber::new(2).unwrap(),
        group_count: 1,
        mounted: true,
        journal_sb_block_start: None,
    }
}

fn create_test_dir_inode() -> Ext4Inode {
    let mut inode = Ext4Inode {
        i_mode: 0x4000 | 0o755,
        i_uid: 0,
        i_size_lo: 4096,
        i_atime: 0,
        i_ctime: 0,
        i_mtime: 0,
        i_dtime: 0,
        i_gid: 0,
        i_links_count: 2,
        i_blocks_lo: 8,
        i_flags: Ext4Inode::EXT4_INDEX_FL,
        l_i_version: 0,
        i_block: [0; 15],
        i_generation: 0,
        i_file_acl_lo: 0,
        i_size_high: 0,
        i_obso_faddr: 0,
        l_i_blocks_high: 0,
        l_i_file_acl_high: 0,
        l_i_uid_high: 0,
        l_i_gid_high: 0,
        l_i_checksum_lo: 0,
        l_i_reserved: 0,
        i_extra_isize: 32,
        i_checksum_hi: 0,
        i_ctime_extra: 0,
        i_mtime_extra: 0,
        i_atime_extra: 0,
        i_crtime: 0,
        i_crtime_extra: 0,
        i_version_hi: 0,
        i_projid: 0,
    };

    inode.write_extend_header();
    inode
}

#[test]
fn test_hash_tree_manager_creation() {
    let fs = create_test_fs();
    let manager = create_hash_tree_manager(&fs);

    assert_eq!(
        manager.hash_seed,
        [0x12345678, 0x87654321, 0xABCDEF00, 0x00FEDCBA]
    );
    assert_eq!(manager.hash_version, 0x8);
    assert_eq!(manager.indirect_levels, 0);
}

#[test]
fn test_htree_hash_calculation() {
    let test_cases = [
        ("test.txt", 0),
        ("file1.bin", 1),
        ("directory", 2),
        ("", 0),
        ("a", 0),
    ];
    let seed = [0x12345678, 0x87654321, 0xABCDEF00, 0x00FEDCBA];

    for (name, version) in test_cases {
        let hash = crate::entries::htree_dir::calculate_hash(name.as_bytes(), version, &seed);
        if !name.is_empty() {
            assert_ne!(hash, 0, "Hash for '{}' should not be zero", name);
        }
    }
}

#[test]
fn test_inode_htree_check() {
    let mut inode = create_test_dir_inode();
    assert!(inode.is_htree_indexed());

    inode.i_flags &= !Ext4Inode::EXT4_INDEX_FL;
    assert!(!inode.is_htree_indexed());

    inode.i_mode = 0x8000 | 0o644;
    assert!(!inode.is_htree_indexed());
}

#[test]
fn test_dx_entry_parsing() {
    let fs = create_test_fs();
    let manager = create_hash_tree_manager(&fs);

    let mut test_data = vec![0; 16];
    test_data[0..4].copy_from_slice(&0x12345678u32.to_le_bytes());
    test_data[4..8].copy_from_slice(&1u32.to_le_bytes());
    test_data[8..12].copy_from_slice(&0x87654321u32.to_le_bytes());
    test_data[12..16].copy_from_slice(&2u32.to_le_bytes());

    let entries = manager.parse_dx_entries(&test_data).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].hash, 0x12345678);
    assert_eq!(entries[0].block, 1);
    assert_eq!(entries[1].hash, 0x87654321);
    assert_eq!(entries[1].block, 2);
}

#[test]
fn test_hash_tree_node_types() {
    let root_node = HashTreeNode::Root {
        hash_version: 0x8,
        indirect_levels: 1,
        entries: Vec::new(),
    };
    match root_node {
        HashTreeNode::Root {
            hash_version,
            indirect_levels,
            ..
        } => {
            assert_eq!(hash_version, 0x8);
            assert_eq!(indirect_levels, 1);
        }
        _ => panic!("Expected root node"),
    }

    let leaf_node = HashTreeNode::Leaf {
        block_num: AbsoluteBN::new(42),
        entries: Vec::new(),
    };
    match leaf_node {
        HashTreeNode::Leaf { block_num, entries } => {
            assert_eq!(block_num, AbsoluteBN::new(42));
            assert!(entries.is_empty());
        }
        _ => panic!("Expected leaf node"),
    }
}

#[test]
fn test_fallback_to_linear_search() {
    let mut fs = create_test_fs();
    let manager = create_hash_tree_manager(&fs);
    let mut dir_inode = create_test_dir_inode();

    let mut mock_device = MockBlockDevice::new(1024 * 1024);
    mock_device.open().unwrap();
    let mut mock_dev = Jbd2Dev::initial_jbd2dev(0, mock_device, false);
    dir_inode.write_extend_header();
    dir_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;

    let result =
        manager.fallback_to_linear_search(&mut fs, &mut mock_dev, &dir_inode, b"nonexistent.txt");
    assert!(matches!(result, Err(HashTreeError::EntryNotFound)));
}
