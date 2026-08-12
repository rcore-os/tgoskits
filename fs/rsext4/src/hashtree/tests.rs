use alloc::{vec, vec::Vec};
use core::cell::Cell;

use super::{hash::DirectoryHash, *};
use crate::{
    blockdev::{BlockIo, Jbd2Dev},
    bmalloc::{BlockAllocator, InodeAllocator, InodeNumber},
    config::GOOD_OLD_INODE_SIZE,
    disknode::{Ext4Inode, Ext4Timestamp},
    entries::{Ext4DirEntry2, Ext4DxRootInfo},
    error::Ext4Error,
    ext4::{Ext4FileSystem, SystemZoneMap},
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

impl BlockIo for MockBlockDevice {
    fn write(
        &mut self,
        buffer: &[u8],
        block_id: crate::io::SectorId,
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

        self.data[start..end].copy_from_slice(buffer);
        Ok(())
    }

    fn read(
        &mut self,
        buffer: &mut [u8],
        block_id: crate::io::SectorId,
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

    fn geometry(&self) -> crate::io::DeviceGeometry {
        crate::io::DeviceGeometry::new(512, (self.data.len() / 512) as u64)
    }

    fn capabilities(&self) -> crate::io::DeviceCapabilities {
        crate::io::DeviceCapabilities {
            read_only: { false },

            flush: true,

            ..crate::io::DeviceCapabilities::default()
        }
    }

    fn flush(&mut self) -> crate::Ext4Result<()> {
        Ok(())
    }
}

impl crate::runtime::Clock for MockBlockDevice {
    fn now(&self) -> Result<Ext4Timestamp, Ext4Error> {
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
        s_def_hash_version: Ext4DxRootInfo::DX_HASH_HALF_MD4,
        ..Default::default()
    };

    let inode_size = match superblock.s_inode_size {
        0 => GOOD_OLD_INODE_SIZE as usize,
        n => n as usize,
    };

    Ext4FileSystem {
        superblock,
        superblock_dirty: false,
        group_descs: Vec::new(),
        dirty_group_descs: Vec::new(),
        block_allocator: BlockAllocator::new(&superblock),
        inode_allocator: InodeAllocator::new(&superblock),
        bitmap_cache: BitmapCache::new(100),
        inodetable_cache: InodeCache::new(100, inode_size),
        datablock_cache: DataBlockCache::new(100, 4096),
        root_inode: InodeNumber::new(2).unwrap(),
        group_count: 1,
        mounted: true,
        journal_sb_block_start: None,
        system_zones: SystemZoneMap::default(),
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

fn linux_htree_root_block(has_metadata_checksum: bool) -> Vec<u8> {
    let mut root = vec![0_u8; 4096];
    root[0..4].copy_from_slice(&2_u32.to_le_bytes());
    root[4..6].copy_from_slice(&12_u16.to_le_bytes());
    root[6] = 1;
    root[7] = Ext4DirEntry2::EXT4_FT_DIR;
    root[8] = b'.';
    root[12..16].copy_from_slice(&2_u32.to_le_bytes());
    root[16..18].copy_from_slice(&(4096_u16 - 12).to_le_bytes());
    root[18] = 2;
    root[19] = Ext4DirEntry2::EXT4_FT_DIR;
    root[20..22].copy_from_slice(b"..");
    root[28] = Ext4DxRootInfo::DX_HASH_HALF_MD4;
    root[29] = Ext4DxRootInfo::INFO_LENGTH;
    let limit = if has_metadata_checksum { 507 } else { 508 };
    root[32..34].copy_from_slice(&limit_u16(limit).to_le_bytes());
    root[34..36].copy_from_slice(&1_u16.to_le_bytes());
    root[36..40].copy_from_slice(&1_u32.to_le_bytes());
    root
}

fn limit_u16(limit: usize) -> u16 {
    u16::try_from(limit).unwrap()
}

#[test]
fn test_hash_tree_manager_creation() {
    let fs = create_test_fs();
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);

    assert_eq!(
        manager.hash_seed,
        [0x12345678, 0x87654321, 0xABCDEF00, 0x00FEDCBA]
    );
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
        let hash = calculate_hash(name.as_bytes(), version, &seed)
            .unwrap()
            .major;
        if !name.is_empty() {
            assert_ne!(hash, 0, "Hash for '{}' should not be zero", name);
        }
    }
}

#[test]
fn linux_directory_hash_matches_debugfs_vectors() {
    let seed = [0; 4];
    let vectors = [
        (0, 0x75af_d992, 0),
        (1, 0xd196_a868, 0xc420_eb28),
        (2, 0xb143_5ec4, 0x3f7e_aa0e),
    ];
    for (version, major, minor) in vectors {
        assert_eq!(
            calculate_hash(b"abc", version, &seed).unwrap(),
            DirectoryHash { major, minor }
        );
    }

    let utf8_vectors = [
        (0, 0x1108_3c86, 0),
        (1, 0x89d4_704e, 0x75d5_2d82),
        (2, 0x591e_9bd6, 0xf780_721f),
        (3, 0x878c_a486, 0),
        (4, 0xfda9_f3f8, 0x6978_8442),
        (5, 0x6daf_7c00, 0xdc9b_6b19),
    ];
    for (version, major, minor) in utf8_vectors {
        assert_eq!(
            calculate_hash("é".as_bytes(), version, &seed).unwrap(),
            DirectoryHash { major, minor }
        );
    }

    let uuid_seed = [0x7856_3412, 0x7856_3412, 0x2143_6587, 0xbadc_fe00];
    assert_eq!(
        calculate_hash(b"abc", 1, &uuid_seed).unwrap(),
        DirectoryHash {
            major: 0xf73a_0418,
            minor: 0x009b_4223,
        }
    );
    assert_eq!(
        calculate_hash(b"abc", 2, &uuid_seed).unwrap(),
        DirectoryHash {
            major: 0xd1bc_32b2,
            minor: 0xefdf_8282,
        }
    );
    assert_eq!(
        calculate_hash(b"abc", Ext4DxRootInfo::DX_HASH_SIPHASH, &seed),
        Err(HashTreeError::UnsupportedHashVersion)
    );
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
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);

    let mut test_data = vec![0; 16];
    test_data[0..2].copy_from_slice(&2_u16.to_le_bytes());
    test_data[2..4].copy_from_slice(&2_u16.to_le_bytes());
    test_data[4..8].copy_from_slice(&1u32.to_le_bytes());
    test_data[8..12].copy_from_slice(&0x87654321u32.to_le_bytes());
    test_data[12..16].copy_from_slice(&2u32.to_le_bytes());

    let entries = manager.parse_dx_entries(&test_data, 0, 2).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].hash, 0);
    assert_eq!(entries[0].block, 1);
    assert_eq!(entries[1].hash, 0x87654321);
    assert_eq!(entries[1].block, 2);
}

#[test]
fn linux_htree_root_layout_is_parsed() {
    let fs = create_test_fs();
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let root = linux_htree_root_block(false);

    let node = manager.parse_root_node(&root, false, 1).unwrap();
    match node {
        HashTreeNode::Root {
            hash_version,
            indirect_levels,
            entries,
        } => {
            assert_eq!(hash_version, Ext4DxRootInfo::DX_HASH_HALF_MD4);
            assert_eq!(indirect_levels, 0);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].hash, 0);
            assert_eq!(entries[0].block, 1);
        }
        _ => panic!("expected root node"),
    }
}

#[test]
fn htree_parser_rejects_invalid_count_order_and_root_geometry() {
    let fs = create_test_fs();
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);

    let mut invalid_root = linux_htree_root_block(false);
    invalid_root[34..36].copy_from_slice(&0_u16.to_le_bytes());
    assert!(matches!(
        manager.parse_root_node(&invalid_root, false, 1),
        Err(HashTreeError::CorruptedHashTree)
    ));

    let mut invalid_root = linux_htree_root_block(false);
    invalid_root[32..34].copy_from_slice(&507_u16.to_le_bytes());
    assert!(matches!(
        manager.parse_root_node(&invalid_root, false, 1),
        Err(HashTreeError::CorruptedHashTree)
    ));

    let mut invalid_root = linux_htree_root_block(false);
    invalid_root[16..18].copy_from_slice(&12_u16.to_le_bytes());
    assert!(matches!(
        manager.parse_root_node(&invalid_root, false, 1),
        Err(HashTreeError::CorruptedHashTree)
    ));

    let mut unordered = [0_u8; 24];
    unordered[0..2].copy_from_slice(&3_u16.to_le_bytes());
    unordered[2..4].copy_from_slice(&3_u16.to_le_bytes());
    unordered[4..8].copy_from_slice(&1_u32.to_le_bytes());
    unordered[8..12].copy_from_slice(&20_u32.to_le_bytes());
    unordered[12..16].copy_from_slice(&2_u32.to_le_bytes());
    unordered[16..20].copy_from_slice(&10_u32.to_le_bytes());
    unordered[20..24].copy_from_slice(&3_u32.to_le_bytes());
    assert!(matches!(
        manager.parse_dx_entries(&unordered, 0, 3),
        Err(HashTreeError::CorruptedHashTree)
    ));
}

#[test]
fn htree_parser_accounts_for_dx_tail_and_internal_layout() {
    let fs = create_test_fs();
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let root = linux_htree_root_block(true);
    assert!(manager.parse_root_node(&root, true, 1).is_ok());

    let mut internal = vec![0_u8; 4096];
    internal[4..6].copy_from_slice(&4096_u16.to_le_bytes());
    internal[8..10].copy_from_slice(&510_u16.to_le_bytes());
    internal[10..12].copy_from_slice(&1_u16.to_le_bytes());
    internal[12..16].copy_from_slice(&2_u32.to_le_bytes());
    assert!(manager.parse_internal_node(&internal, true).is_ok());

    internal[8..10].copy_from_slice(&511_u16.to_le_bytes());
    assert!(matches!(
        manager.parse_internal_node(&internal, true),
        Err(HashTreeError::CorruptedHashTree)
    ));
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
}

#[test]
fn test_fallback_to_linear_search() {
    let mut fs = create_test_fs();
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let mut dir_inode = create_test_dir_inode();

    let mock_device = MockBlockDevice::new(1024 * 1024);
    let mut mock_dev = Jbd2Dev::initial_jbd2dev(0, mock_device, false);
    dir_inode.write_extend_header();
    dir_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;

    let dir_ino = fs.root_inode;
    let result = manager.fallback_to_linear_search(
        &mut fs,
        &mut mock_dev,
        dir_ino,
        &dir_inode,
        b"nonexistent.txt",
    );
    assert!(matches!(result, Err(HashTreeError::EntryNotFound)));
}

#[test]
fn linear_fallback_preserves_extent_codec_error() {
    let mut fs = create_test_fs();
    let manager = HashTreeManager::new(fs.superblock.s_hash_seed);
    let mut dir_inode = create_test_dir_inode();
    dir_inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
    dir_inode.i_block[0] = 0;

    let mock_device = MockBlockDevice::new(1024 * 1024);
    let mut mock_dev = Jbd2Dev::initial_jbd2dev(0, mock_device, false);
    let result = manager.fallback_to_linear_search(
        &mut fs,
        &mut mock_dev,
        InodeNumber::new(2).unwrap(),
        &dir_inode,
        b"nonexistent.txt",
    );

    assert!(matches!(
        result,
        Err(HashTreeError::Filesystem(error))
            if error.kind() == crate::error::Ext4ErrorKind::Corrupted
    ));
}
