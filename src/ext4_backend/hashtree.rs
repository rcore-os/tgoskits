//! Directory entry HashTree implementation
//!
//! Provides hash tree-based directory lookup functionality, replacing linear search to improve performance for large directories
//! Supports Ext4 HTree index format, including multiple hash algorithms

use crate::ext4_backend::blockdev::*;
use crate::ext4_backend::config::*;
use crate::ext4_backend::disknode::*;
use crate::ext4_backend::endian::*;
use crate::ext4_backend::entries::*;
use crate::ext4_backend::ext4::*;
use crate::ext4_backend::loopfile::*;

use alloc::vec::Vec;
use log::error;
use log::{debug,  warn};

/// Hash tree error type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashTreeError {
    /// Invalid hash tree format
    InvalidHashTree,
    /// Unsupported hash version
    UnsupportedHashVersion,
    /// Corrupted hash tree
    CorruptedHashTree,
    /// Block number out of range
    BlockOutOfRange,
    /// Buffer too small
    BufferTooSmall,
    /// Entry not found
    EntryNotFound,
}

impl core::fmt::Display for HashTreeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HashTreeError::InvalidHashTree => write!(f, "Invalid hash tree format"),
            HashTreeError::UnsupportedHashVersion => write!(f, "Unsupported hash version"),
            HashTreeError::CorruptedHashTree => write!(f, "Corrupted hash tree"),
            HashTreeError::BlockOutOfRange => write!(f, "Block number out of range"),
            HashTreeError::BufferTooSmall => write!(f, "Buffer too small"),
            HashTreeError::EntryNotFound => write!(f, "Entry not found"),
        }
    }
}

/// Hash tree search result
#[derive(Debug)]
pub struct HashTreeSearchResult {
    /// Found directory entry
    pub entry: Ext4DirEntryInfo<'static>,
    /// Block number where entry is located
    pub block_num: u32,
    /// Offset within the block
    pub offset: usize,
}

/// Hash tree manager
pub struct HashTreeManager {
    /// Hash seed (from superblock)
    hash_seed: [u32; 4],
    /// Hash version
    hash_version: u8,
    /// Number of indirect levels
    indirect_levels: u8,
}

impl HashTreeManager {
    /// Create new hash tree manager
    pub fn new(hash_seed: [u32; 4], hash_version: u8, indirect_levels: u8) -> Self {
        Self {
            hash_seed,
            hash_version,
            indirect_levels,
        }
    }

    /// Search for filename in directory (using hash tree)
    pub fn lookup<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_inode: &Ext4Inode,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        debug!(
            "Starting hash tree lookup: {:?}",
            core::str::from_utf8(target_name)
        );

        // 1. Check if directory has hash tree index enabled
        if !dir_inode.is_htree_indexed() {
           //warn!("Directory does not have hash tree index enabled, falling back to linear search");
            return self.fallback_to_linear_search(fs, block_dev, dir_inode, target_name);
        }

        // 2. Calculate hash value of target filename
        let target_hash =
            htree_dir::calculate_hash(target_name, self.hash_version, &self.hash_seed);
        debug!("Target hash value: 0x{target_hash:08x}");

        // 3. Read root node
        let root_block = self.get_root_block( block_dev, dir_inode)?;
        let root_data = self.read_block_data(fs, block_dev, root_block)?;

        // 4. Parse root node
        let root_info = self.parse_root_node(&root_data)?;

        // 5. Search in hash tree
        match self.search_in_hash_tree(fs, block_dev, &root_info, target_hash, target_name) {
            Ok(result) => Ok(result),
            Err(e) => {
                warn!(
                    "Hash tree lookup failed: {e}, falling back to linear search"
                );
                self.fallback_to_linear_search(fs, block_dev, dir_inode, target_name)
            }
        }
    }

    /// Get hash tree root block number
    fn get_root_block<B: BlockDevice>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        dir_inode: &Ext4Inode,
    ) -> Result<u32, HashTreeError> {
        // Root block is usually the first data block of the directory
        match resolve_inode_block(block_dev, &mut dir_inode.clone(), 0) {
            Ok(Some(block)) => Ok(block),
            Ok(None) => Err(HashTreeError::InvalidHashTree),
            Err(_) => Err(HashTreeError::BlockOutOfRange),
        }
    }

    /// Read block data
    fn read_block_data<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        block_num: u32,
    ) -> Result<Vec<u8>, HashTreeError> {
        match fs.datablock_cache.get_or_load(block_dev, block_num as u64) {
            Ok(cached_block) => Ok(cached_block.data.clone()),
            Err(_) => Err(HashTreeError::BlockOutOfRange),
        }
    }

    /// Parse root node
    fn parse_root_node(&self, data: &[u8]) -> Result<HashTreeNode, HashTreeError> {
        if data.len() < core::mem::size_of::<Ext4DxRoot>() {
            return Err(HashTreeError::BufferTooSmall);
        }

        // Parse root node info
        let dot = Ext4DirEntryInfo::parse_from_bytes(&data[0..8])
            .ok_or(HashTreeError::CorruptedHashTree)?;

        let dotdot = Ext4DirEntryInfo::parse_from_bytes(&data[dot.inode as usize..])
            .ok_or(HashTreeError::CorruptedHashTree)?;

        // Extract root info
        let info_offset = dot.inode as usize + dotdot.inode as usize;
        if info_offset + core::mem::size_of::<Ext4DxRootInfo>() > data.len() {
            return Err(HashTreeError::CorruptedHashTree);
        }

        let info_bytes = &data[info_offset..info_offset + core::mem::size_of::<Ext4DxRootInfo>()];
        let hash_version = info_bytes[5]; // hash_version field is at offset 5
        let indirect_levels = info_bytes[6]; // indirect_levels field is at offset 6

        // Parse hash entries
        let entries_offset = info_offset + core::mem::size_of::<Ext4DxRootInfo>();
        let entries = self.parse_dx_entries(&data[entries_offset..])?;

        Ok(HashTreeNode::Root {
            hash_version,
            indirect_levels,
            entries,
        })
    }

    /// Parse DX entry array
    fn parse_dx_entries(&self, data: &[u8]) -> Result<Vec<Ext4DxEntry>, HashTreeError> {
        let mut entries = Vec::new();
        let mut offset = 0;

        while offset + core::mem::size_of::<Ext4DxEntry>() <= data.len() {
            let hash = read_u32_le(&data[offset..offset + 4]);
            let block = read_u32_le(&data[offset + 4..offset + 8]);

            if block == 0 {
                break;
            }

            entries.push(Ext4DxEntry { hash, block });
            offset += core::mem::size_of::<Ext4DxEntry>();
        }

        Ok(entries)
    }

    /// Search in hash tree
    fn search_in_hash_tree<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        node: &HashTreeNode,
        target_hash: u32,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        match node {
            HashTreeNode::Root { entries, .. } => {
                self.search_in_entries(fs, block_dev, entries, target_hash, target_name, 0)
            }
            HashTreeNode::Internal { entries, .. } => {
                self.search_in_entries(fs, block_dev, entries, target_hash, target_name, 0)
            }
            HashTreeNode::Leaf { block_num, .. } => {
                self.search_in_leaf_block(fs, block_dev, *block_num, target_name)
            }
        }
    }

    /// Search in entry list
    fn search_in_entries<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        entries: &[Ext4DxEntry],
        target_hash: u32,
        target_name: &[u8],
        level: u32,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        // Find appropriate entry (largest entry with hash <= target hash)
        let mut selected_entry = None;
        for entry in entries {
            if entry.hash <= target_hash {
                selected_entry = Some(entry);
            } else {
                break;
            }
        }

        let entry = selected_entry.ok_or(HashTreeError::EntryNotFound)?;

        // Read target block
        let block_data = self.read_block_data(fs, block_dev, entry.block)?;

        // Check if this is a leaf node
        if level >= self.indirect_levels as u32 {
            // Leaf node, search for specific directory entries within it
            self.search_in_leaf_data(&block_data, target_name, entry.block)
        } else {
            // Internal node, recursive search
            let internal_node = self.parse_internal_node(&block_data)?;
            self.search_in_hash_tree(fs, block_dev, &internal_node, target_hash, target_name)
        }
    }

    /// Search in leaf block
    fn search_in_leaf_block<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        block_num: u32,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let block_data = self.read_block_data(fs, block_dev, block_num)?;
        self.search_in_leaf_data(&block_data, target_name, block_num)
    }

    /// Search in leaf data
    fn search_in_leaf_data(
        &self,
        data: &[u8],
        target_name: &[u8],
        block_num: u32,
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        let iter = DirEntryIterator::new(data);

        for (entry, offset) in iter {
            if entry.name == target_name {
                return Ok(HashTreeSearchResult {
                    entry: unsafe { core::mem::transmute(entry) },
                    block_num,
                    offset: offset as usize,
                });
            }
        }

        Err(HashTreeError::EntryNotFound)
    }

    /// Parse internal node
    fn parse_internal_node(&self, data: &[u8]) -> Result<HashTreeNode, HashTreeError> {
        if data.len() < core::mem::size_of::<Ext4DxNode>() {
            return Err(HashTreeError::BufferTooSmall);
        }

        // Skip fake directory entries
        let fake_entry_size = core::mem::size_of::<Ext4DirEntry2>();
        let countlimit_offset = fake_entry_size;

        if countlimit_offset + core::mem::size_of::<Ext4DxCountlimit>() > data.len() {
            return Err(HashTreeError::CorruptedHashTree);
        }

        let countlimit_bytes =
            &data[countlimit_offset..countlimit_offset + core::mem::size_of::<Ext4DxCountlimit>()];
        let _count = read_u16_le(&countlimit_bytes[2..4]) as usize; // count field is at offset 2

        // Parse entries
        let entries_offset = countlimit_offset + core::mem::size_of::<Ext4DxCountlimit>();
        let entries = self.parse_dx_entries(&data[entries_offset..])?;

        Ok(HashTreeNode::Internal {
            entries,
            level: 0, // Should actually read from node
        })
    }

    /// Fall back to linear search
    fn fallback_to_linear_search<B: BlockDevice>(
        &self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        dir_inode: &Ext4Inode,
        target_name: &[u8],
    ) -> Result<HashTreeSearchResult, HashTreeError> {
        debug!(
            "Using linear search: {:?}",
            core::str::from_utf8(target_name)
        );

        let total_size = dir_inode.size() as usize;
        let block_bytes = BLOCK_SIZE;
        let total_blocks = if total_size == 0 {
            0
        } else {
            total_size.div_ceil(block_bytes)
        };

        // Fast path for extent-based directories: resolve all blocks once, then scan.
        if dir_inode.have_extend_header_and_use_extend() {
            let mut inode_clone = dir_inode.clone();
            let blocks_map = match resolve_inode_block_allextend(fs, block_dev, &mut inode_clone) {
                Ok(v) => v,
                Err(_) => return Err(HashTreeError::BlockOutOfRange),
            };

            for lbn in 0..total_blocks {
                let phys = match blocks_map.get(&(lbn as u32)) {
                    Some(v) => *v,
                    None => continue,
                };

                let cached_block = match fs.datablock_cache.get_or_load(block_dev, phys as u64) {
                    Ok(block) => block,
                    Err(_) => return Err(HashTreeError::BlockOutOfRange),
                };

                let block_data = &cached_block.data[..block_bytes];
                if let Some(entry) = classic_dir::find_entry(block_data, target_name) {
                    return Ok(HashTreeSearchResult {
                        entry: unsafe { core::mem::transmute(entry) },
                        block_num: phys as u32,
                        offset: 0,
                    });
                }
            }
            
            return Err(HashTreeError::EntryNotFound);
        }

        error!("FS NOT SUPPORT NORMAL MULTIPUL POINTER ,PLEASE TURN ON EXTEND FEATURE!");
        Err(HashTreeError::CorruptedHashTree)
    }
}

/// Hash tree node type
#[derive(Debug)]
pub enum HashTreeNode {
    /// Root node
    Root {
        hash_version: u8,
        indirect_levels: u8,
        entries: Vec<Ext4DxEntry>,
    },
    /// Internal node
    Internal {
        entries: Vec<Ext4DxEntry>,
        level: u32,
    },
    /// Leaf node
    Leaf {
        block_num: u32,
        entries: Vec<Ext4DirEntryInfo<'static>>,
    },
}

/// Extend Ext4Inode to support hash tree checking
pub trait Ext4InodeHashTreeExt {
    /// Check if inode has hash tree index enabled
    fn is_htree_indexed(&self) -> bool;

    /// Get directory hash tree root info
    fn get_htree_root_info(&self) -> Option<(u8, u8)>; // (hash_version, indirect_levels)
}

impl Ext4InodeHashTreeExt for Ext4Inode {
    fn is_htree_indexed(&self) -> bool {
        // Check if inode flags contain indexed directory flag
        self.i_flags & Self::EXT4_INDEX_FL != 0
    }

    fn get_htree_root_info(&self) -> Option<(u8, u8)> {
        if !self.is_htree_indexed() {
            return None;
        }

        Some((htree_dir::calculate_hash(b"", 0, &[0; 4]) as u8, 0))
    }
}

/// Create hash tree manager
pub fn create_hash_tree_manager(fs: &Ext4FileSystem) -> HashTreeManager {
    HashTreeManager::new(
        fs.superblock.s_hash_seed,
        fs.superblock.s_def_hash_version,
        0, // indirect_levels, needs to be read from directory inode
    )
}

/// Convenient directory lookup function
pub fn lookup_directory_entry<B: BlockDevice>(
    fs: &mut Ext4FileSystem,
    block_dev: &mut Jbd2Dev<B>,
    dir_inode: &Ext4Inode,
    target_name: &[u8],
) -> Result<HashTreeSearchResult, HashTreeError> {
    let manager = create_hash_tree_manager(fs);
    manager.lookup(fs, block_dev, dir_inode, target_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    use alloc::vec::Vec;
use crate::ext4_backend::error::BlockDevError;
    // Mock block device
    struct MockBlockDevice {
        data: Vec<u8>,
        is_open: bool,
    }

    impl MockBlockDevice {
        fn new(size: usize) -> Self {
            let mut data = Vec::new();
            data.resize(size, 0);
            Self {
                data,
                is_open: false,
            }
        }
    }

    impl BlockDevice for MockBlockDevice {

        fn write(&mut self, buffer: &[u8], block_id: u32, count: u32) -> Result<(), BlockDevError> {
            if !self.is_open {
                return Err(BlockDevError::DeviceNotOpen);
            }

            let start = (block_id as usize) * 512;
            let end = start + (count as usize) * 512;

            if end > self.data.len() {
                return Err(BlockDevError::BlockOutOfRange {
                    block_id,
                    max_blocks: (self.data.len() / 512) as u64,
                });
            }

            self.data[start..end].copy_from_slice(buffer);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: u32, count: u32) -> Result<(), BlockDevError> {
            if !self.is_open {
                return Err(BlockDevError::DeviceNotOpen);
            }

            let start = (block_id as usize) * 512;
            let end = start + (count as usize) * 512;

            if end > self.data.len() {
                return Err(BlockDevError::BlockOutOfRange {
                    block_id,
                    max_blocks: (self.data.len() / 512) as u64,
                });
            }

            buffer.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn open(&mut self) -> Result<(), BlockDevError> {
            self.is_open = true;
            Ok(())
        }

        fn close(&mut self) -> Result<(), BlockDevError> {
            self.is_open = false;
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            (self.data.len() / 512) as u64
        }
    }

    // Create test filesystem
    fn create_test_fs() -> Ext4FileSystem {
        use crate::ext4_backend::superblock::Ext4Superblock;
        use crate::ext4_backend::inodetable_cache::InodeCache;
        use crate::ext4_backend::datablock_cache::DataBlockCache;
        use crate::ext4_backend::bitmap_cache::BitmapCache;
        use crate::ext4_backend::bmalloc::*;
        let mut superblock = Ext4Superblock::default();
        superblock.s_hash_seed = [0x12345678, 0x87654321, 0xABCDEF00, 0x00FEDCBA];
        superblock.s_def_hash_version = 0x8; // Half SipHash

        let inode_size = match superblock.s_inode_size {
            0 => crate::ext4_backend::config::DEFAULT_INODE_SIZE as usize,
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
            root_inode: 2,
            group_count: 1,
            mounted: true,
            journal_sb_block_start: None,
        }
    }

    // Create test directory inode
    fn create_test_dir_inode() -> Ext4Inode {
        let mut inode = Ext4Inode {
            i_mode: 0x4000 | 0o755, // Directory type
            i_uid: 0,
            i_size_lo: 4096,
            i_atime: 0,
            i_ctime: 0,
            i_mtime: 0,
            i_dtime: 0,
            i_gid: 0,
            i_links_count: 2,
            i_blocks_lo: 8,
            i_flags: Ext4Inode::EXT4_INDEX_FL, // Enable index flag
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
            i_extra_isize: 0,
            i_checksum_hi: 0,
            i_ctime_extra: 0,
            i_mtime_extra: 0,
            i_atime_extra: 0,
            i_crtime: 0,
            i_crtime_extra: 0,
            i_version_hi: 0,
            i_projid: 0,
        };

        // Provide a valid direct block mapping for lbn=0, so linear search can read the block
        // instead of failing in resolve_inode_block due to a 0/invalid physical address.

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
            ("test.txt", 0),  // Using legacy hash
            ("file1.bin", 1), // Using Half MD4
            ("directory", 2), // Using TEA
            ("", 0),          // Empty string
            ("a", 0),         // Single character
        ];

        let seed = [0x12345678, 0x87654321, 0xABCDEF00, 0x00FEDCBA];

        for (name, version) in test_cases {
            let hash = htree_dir::calculate_hash(name.as_bytes(), version, &seed);
            // Verify hash value is not 0 (unless it is an empty string)
            if !name.is_empty() {
                assert_ne!(hash, 0, "Hash for '{}' should not be zero", name);
            }
        }
    }

    #[test]
    fn test_inode_htree_check() {
        let mut inode = create_test_dir_inode();

        // Test directory with index enabled
        assert!(inode.is_htree_indexed());

        // Test directory without index enabled
        inode.i_flags &= !Ext4Inode::EXT4_INDEX_FL;
        assert!(!inode.is_htree_indexed());

        // Test non-directory inode
        inode.i_mode = 0x8000 | 0o644; // Regular file
        assert!(!inode.is_htree_indexed());
    }

    #[test]
    fn test_dx_entry_parsing() {
        let fs = create_test_fs();
        let manager = create_hash_tree_manager(&fs);

        // Create test data: two DX entries
        let mut test_data = Vec::new();
        test_data.resize(16, 0);
        // First entry: hash=0x12345678, block=1
        test_data[0..4].copy_from_slice(&0x12345678u32.to_le_bytes());
        test_data[4..8].copy_from_slice(&1u32.to_le_bytes());
        // Second entry: hash=0x87654321, block=2
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
        // Test root node
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

        // Test leaf node
        let leaf_node = HashTreeNode::Leaf {
            block_num: 42,
            entries: Vec::new(),
        };
        match leaf_node {
            HashTreeNode::Leaf { block_num, entries } => {
                assert_eq!(block_num, 42);
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

        // Create a mock block device
        let mut mock_device = MockBlockDevice::new(1024 * 1024);
        mock_device.open().unwrap();
        let mut mock_dev = Jbd2Dev::initial_jbd2dev(0, mock_device, false);
        dir_inode.write_extend_header();
        dir_inode.i_flags |=Ext4Inode::EXT4_EXTENTS_FL;
        let result = manager.fallback_to_linear_search(
            &mut fs,
            &mut mock_dev,
            &dir_inode,
            b"nonexistent.txt",
        );

        assert!(matches!(result, Err(HashTreeError::EntryNotFound)));
    }
}
