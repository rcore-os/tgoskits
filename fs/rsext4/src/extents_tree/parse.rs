use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtentRun {
    pub logical_start: u32,
    pub physical_start: AbsoluteBN,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtentBlockMapping {
    Hole,
    Initialized(AbsoluteBN),
    Unwritten(AbsoluteBN),
}

impl<'a> ExtentTree<'a> {
    pub const MAX_DEPTH: u16 = 32;

    pub fn parse_node(bytes: &[u8]) -> Ext4Result<ExtentNode> {
        Self::parse_node_from_bytes(bytes)
    }

    /// Parses one extent-tree node from raw bytes.
    pub(super) fn parse_node_from_bytes(bytes: &[u8]) -> Ext4Result<ExtentNode> {
        let hdr_size = Ext4ExtentHeader::disk_size();
        if bytes.len() < hdr_size {
            return Err(Ext4Error::corrupted().with_operation("extent:header_truncated"));
        }

        let header = Ext4ExtentHeader::from_disk_bytes(&bytes[..hdr_size]);
        if header.eh_magic != Ext4ExtentHeader::EXT4_EXT_MAGIC {
            return Err(Ext4Error::corrupted().with_operation("extent:bad_magic"));
        }

        let entries = header.eh_entries as usize;
        let max = header.eh_max as usize;
        let entry_size = Ext4Extent::disk_size();
        let capacity = bytes.len().saturating_sub(hdr_size) / entry_size;
        if max == 0 || entries > max || max > capacity || header.eh_depth > Self::MAX_DEPTH {
            return Err(Ext4Error::corrupted().with_operation("extent:header_geometry"));
        }
        if header.eh_depth > 0 && entries == 0 {
            return Err(Ext4Error::corrupted().with_operation("extent:empty_index"));
        }
        if header.eh_depth != 0 && entries == 0 {
            error!("Extent internal node has no child indexes");
            return None;
        }

        let mut offset = hdr_size;

        if header.eh_depth == 0 {
            // Leaf nodes store extents directly.
            let mut vec = Vec::with_capacity(entries);
            let et_size = Ext4Extent::disk_size();
            for _ in 0..entries {
                if offset + et_size > bytes.len() {
                    return Err(Ext4Error::corrupted().with_operation("extent:entry_truncated"));
                }
                let et = Ext4Extent::from_disk_bytes(&bytes[offset..offset + et_size]);
                vec.push(et);
                offset += et_size;
            }
            Self::validate_leaf_entries(&vec)?;
            Ok(ExtentNode::Leaf {
                header,
                entries: vec,
            })
        } else {
            // Internal nodes store child indexes.
            let mut vec = Vec::with_capacity(entries);
            let idx_size = Ext4ExtentIdx::disk_size();
            for _ in 0..entries {
                if offset + idx_size > bytes.len() {
                    return Err(Ext4Error::corrupted().with_operation("extent:index_truncated"));
                }
                let idx = Ext4ExtentIdx::from_disk_bytes(&bytes[offset..offset + idx_size]);
                vec.push(idx);
                offset += idx_size;
            }
            Self::validate_index_entries(&vec)?;
            Ok(ExtentNode::Index {
                header,
                entries: vec,
            })
        }
    }

    pub(super) fn validate_leaf_entries(entries: &[Ext4Extent]) -> Ext4Result<()> {
        let mut previous_end = None;
        for extent in entries {
            let len = extent.len();
            if len == 0 {
                return Err(Ext4Error::corrupted().with_operation("extent:zero_length"));
            }
            let logical_end = extent
                .ee_block
                .checked_add(len)
                .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:logical_overflow"))?;
            extent
                .start_block()
                .checked_add(u64::from(len))
                .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:physical_overflow"))?;
            if previous_end.is_some_and(|end| extent.ee_block < end) {
                return Err(Ext4Error::corrupted().with_operation("extent:overlap_or_order"));
            }
            previous_end = Some(logical_end);
        }
        Ok(())
    }

    pub(super) fn validate_index_entries(entries: &[Ext4ExtentIdx]) -> Ext4Result<()> {
        let mut previous_key = None;
        for index in entries {
            if previous_key.is_some_and(|key| index.ei_block <= key) {
                return Err(Ext4Error::corrupted().with_operation("extent:index_order"));
            }
            previous_key = Some(index.ei_block);
        }
        Ok(())
    }

    /// Finds the extent covering `lblock`, if any.
    pub fn find_extent<B: BlockIo>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        lblock: u32,
    ) -> Ext4Result<Option<Ext4Extent>> {
        let root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, dev.total_blocks(), true)?;
        self.find_in_node(dev, &root, lblock)
    }

    /// Finds the extent covering `lblock`, or the first extent after it.
    pub(crate) fn find_extent_at_or_after<B: BlockIo>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        lblock: u32,
    ) -> Ext4Result<Option<Ext4Extent>> {
        let root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, dev.total_blocks(), true)?;
        self.find_at_or_after_in_node(dev, &root, lblock)
    }

    /// Returns the external leaf block that owns `lblock`; inline leaves have
    /// no block identity.
    pub(crate) fn external_leaf_block<B: BlockIo>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        lblock: u32,
    ) -> Ext4Result<Option<AbsoluteBN>> {
        let root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, dev.total_blocks(), true)?;
        self.external_leaf_block_in_node(dev, &root, lblock, None)
    }

    /// Resolves one logical block without collapsing unwritten extents into holes.
    pub fn map_block<B: BlockIo>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        lblock: u32,
    ) -> Ext4Result<ExtentBlockMapping> {
        let Some(extent) = self.find_extent(dev, lblock)? else {
            return Ok(ExtentBlockMapping::Hole);
        };
        let offset = lblock
            .checked_sub(extent.ee_block)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:mapping_offset"))?;
        let physical = AbsoluteBN::new(extent.start_block()).checked_add(offset)?;
        Ok(if extent.is_unwritten() {
            ExtentBlockMapping::Unwritten(physical)
        } else {
            ExtentBlockMapping::Initialized(physical)
        })
    }

    pub fn initialized_runs_in_range<B: BlockIo>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        start_lbn: u32,
        end_lbn: u32,
    ) -> Ext4Result<Vec<ExtentRun>> {
        if start_lbn > end_lbn {
            return Ok(Vec::new());
        }
        let root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, dev.total_blocks(), true)?;
        let mut runs = Vec::new();
        self.collect_runs_in_node(dev, &root, start_lbn, end_lbn, &mut runs)?;
        Ok(runs)
    }

    /// Recursively searches one node for the extent covering `lblock`.
    #[allow(clippy::only_used_in_recursion)]
    fn find_in_node<B: BlockIo>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        lblock: u32,
    ) -> Ext4Result<Option<Ext4Extent>> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for et in entries {
                    let start = et.ee_block;
                    let len = et.len();
                    let end = start.checked_add(len).ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("extent:logical_overflow")
                    })?; // half-open range [start, end)
                    if lblock >= start && lblock < end {
                        return Ok(Some(*et));
                    }
                }
                Ok(None)
            }
            ExtentNode::Index { header, entries } => {
                // Descend through the last child whose key is <= target.
                let mut chosen = &entries[0];
                for idx in entries {
                    if idx.ei_block <= lblock {
                        chosen = idx;
                    } else {
                        break;
                    }
                }

                let child = self.read_child_node(dev, chosen, header.eh_depth - 1)?;

                self.find_in_node(dev, &child, lblock)
            }
        }
    }

    fn find_at_or_after_in_node<B: BlockIo>(
        &self,
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        lblock: u32,
    ) -> Ext4Result<Option<Ext4Extent>> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for extent in entries {
                    let end = extent.ee_block.checked_add(extent.len()).ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("extent:logical_overflow")
                    })?;
                    if end > lblock {
                        return Ok(Some(*extent));
                    }
                }
                Ok(None)
            }
            ExtentNode::Index { header, entries } => {
                let partition = entries.partition_point(|index| index.ei_block <= lblock);
                let first = partition.saturating_sub(1);
                for index in &entries[first..] {
                    let child = self.read_child_node(dev, index, header.eh_depth - 1)?;
                    if let Some(extent) = self.find_at_or_after_in_node(dev, &child, lblock)? {
                        return Ok(Some(extent));
                    }
                }
                Ok(None)
            }
        }
    }

    fn external_leaf_block_in_node<B: BlockIo>(
        &self,
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        lblock: u32,
        physical_node: Option<AbsoluteBN>,
    ) -> Ext4Result<Option<AbsoluteBN>> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                let owns_block = entries.iter().any(|extent| {
                    extent.ee_block <= lblock
                        && lblock < extent.ee_block.saturating_add(extent.len())
                });
                if owns_block {
                    Ok(physical_node)
                } else {
                    Err(Ext4Error::corrupted().with_operation("extent:leaf_mapping_missing"))
                }
            }
            ExtentNode::Index { header, entries } => {
                let partition = entries.partition_point(|index| index.ei_block <= lblock);
                let position = partition.saturating_sub(1);
                let index = entries
                    .get(position)
                    .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:empty_index"))?;
                let child_block = AbsoluteBN::new(
                    (u64::from(index.ei_leaf_hi) << 32) | u64::from(index.ei_leaf_lo),
                );
                let child = self.read_child_node(dev, index, header.eh_depth - 1)?;
                self.external_leaf_block_in_node(dev, &child, lblock, Some(child_block))
            }
        }
    }

    fn collect_runs_in_node<B: BlockIo>(
        &self,
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        start_lbn: u32,
        end_lbn: u32,
        out: &mut Vec<ExtentRun>,
    ) -> Ext4Result<()> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for ext in entries {
                    let len = ext.len();
                    if len == 0 || ext.is_unwritten() {
                        continue;
                    }
                    let ext_start = ext.ee_block;
                    let ext_end = ext_start
                        .checked_add(len)
                        .and_then(|end| end.checked_sub(1))
                        .ok_or_else(|| {
                            Ext4Error::corrupted().with_operation("extent:logical_overflow")
                        })?;
                    if ext_end < start_lbn || ext_start > end_lbn {
                        continue;
                    }
                    let logical_start = ext_start.max(start_lbn);
                    let logical_end = ext_end.min(end_lbn);
                    let physical_offset = logical_start.saturating_sub(ext_start);
                    let physical_start =
                        AbsoluteBN::new(ext.start_block()).checked_add(physical_offset)?;
                    out.push(ExtentRun {
                        logical_start,
                        physical_start,
                        len: logical_end
                            .checked_sub(logical_start)
                            .and_then(|len| len.checked_add(1))
                            .ok_or_else(Ext4Error::overflow)?,
                    });
                }
                Ok(())
            }
            ExtentNode::Index { header, entries } => {
                for (idx, entry) in entries.iter().enumerate() {
                    let child_start = entry.ei_block;
                    let child_end = entries
                        .get(idx + 1)
                        .map(|next| next.ei_block - 1)
                        .unwrap_or(u32::MAX);
                    if child_end < start_lbn || child_start > end_lbn {
                        continue;
                    }
                    let child = self.read_child_node(dev, entry, header.eh_depth - 1)?;
                    self.collect_runs_in_node(dev, &child, start_lbn, end_lbn, out)?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{vec, vec::Vec};
    use core::cell::Cell;

    use super::*;
    use crate::{
        blockdev::{BlockIo, Jbd2Dev},
        bmalloc::{AbsoluteBN, InodeNumber},
        config::BLOCK_SIZE,
        disknode::Ext4Timestamp,
        error::{ErrorContext, Ext4Error, Ext4Result},
        ext4::{mkfs, mount},
        loopfile::resolve_inode_block,
    };

    struct MemBlockDev {
        data: Vec<u8>,
        total_blocks: u64,
        now: Cell<i64>,
    }

    impl MemBlockDev {
        fn new(total_blocks: u64) -> Self {
            Self {
                data: vec![0; total_blocks as usize * BLOCK_SIZE],
                total_blocks,
                now: Cell::new(1_700_000_000),
            }
        }
    }

    impl BlockIo for MemBlockDev {
        fn write(
            &mut self,
            buffer: &[u8],
            block_id: crate::io::SectorId,
            count: u32,
        ) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            self.data[start..end].copy_from_slice(&buffer[..required]);
            Ok(())
        }

        fn read(
            &mut self,
            buffer: &mut [u8],
            block_id: crate::io::SectorId,
            count: u32,
        ) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            buffer[..required].copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn geometry(&self) -> crate::io::DeviceGeometry {
            crate::io::DeviceGeometry::new(BLOCK_SIZE as u32, self.total_blocks)
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

    impl crate::runtime::Clock for MemBlockDev {
        fn now(&self) -> Ext4Result<Ext4Timestamp> {
            let sec = self.now.get();
            self.now.set(sec + 1);
            Ok(Ext4Timestamp::new(sec, 0))
        }
    }

    fn setup_fs(total_blocks: u64) -> (Jbd2Dev<MemBlockDev>, Ext4FileSystem) {
        let dev = MemBlockDev::new(total_blocks);
        let mut jbd = Jbd2Dev::initial_jbd2dev(0, dev, false);
        mkfs(&mut jbd).unwrap();
        let fs = mount(&mut jbd).unwrap();
        (jbd, fs)
    }

    fn new_extent_inode() -> Ext4Inode {
        let mut inode = Ext4Inode::default();
        inode.i_flags |= Ext4Inode::EXT4_EXTENTS_FL;
        inode.write_extend_header();
        inode
    }

    fn alloc_contiguous<B: BlockIo>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
        count: u32,
    ) -> AbsoluteBN {
        let first = fs.alloc_block(dev).unwrap();
        let mut prev = first;
        for _ in 1..count {
            let next = fs.alloc_block(dev).unwrap();
            assert_eq!(next, prev.checked_add(1).unwrap());
            prev = next;
        }
        first
    }

    fn raw_leaf(extents: &[Ext4Extent], depth: u16) -> [u8; 60] {
        let mut bytes = [0u8; 60];
        Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: extents.len() as u16,
            eh_max: 4,
            eh_depth: depth,
            eh_generation: 0,
        }
        .to_disk_bytes(&mut bytes[..Ext4ExtentHeader::disk_size()]);
        for (slot, extent) in extents.iter().enumerate() {
            let start = Ext4ExtentHeader::disk_size() + slot * Ext4Extent::disk_size();
            extent.to_disk_bytes(&mut bytes[start..start + Ext4Extent::disk_size()]);
        }
        bytes
    }

    #[test]
    fn checked_codec_rejects_unordered_and_overlapping_extents() {
        let unordered = raw_leaf(&[Ext4Extent::new(8, 200, 2), Ext4Extent::new(3, 100, 2)], 0);
        assert!(ExtentTree::parse_node(&unordered).is_err());

        let overlapping = raw_leaf(&[Ext4Extent::new(3, 100, 4), Ext4Extent::new(6, 200, 2)], 0);
        assert!(ExtentTree::parse_node(&overlapping).is_err());

        let mut unordered_index = [0u8; 60];
        Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 2,
            eh_max: 4,
            eh_depth: 1,
            eh_generation: 0,
        }
        .to_disk_bytes(&mut unordered_index[..Ext4ExtentHeader::disk_size()]);
        for (slot, key) in [8, 3].into_iter().enumerate() {
            let offset = Ext4ExtentHeader::disk_size() + slot * Ext4ExtentIdx::disk_size();
            Ext4ExtentIdx {
                ei_block: key,
                ei_leaf_lo: 100 + slot as u32,
                ei_leaf_hi: 0,
                ei_unused: 0,
            }
            .to_disk_bytes(&mut unordered_index[offset..offset + Ext4ExtentIdx::disk_size()]);
        }
        assert!(ExtentTree::parse_node(&unordered_index).is_err());
    }

    #[test]
    fn checked_codec_rejects_zero_length_logical_overflow_and_excess_depth() {
        let zero_length = raw_leaf(
            &[Ext4Extent {
                ee_block: 1,
                ee_len: 0,
                ee_start_hi: 0,
                ee_start_lo: 100,
            }],
            0,
        );
        assert!(ExtentTree::parse_node(&zero_length).is_err());

        let logical_overflow = raw_leaf(&[Ext4Extent::new(u32::MAX, 100, 2)], 0);
        assert!(ExtentTree::parse_node(&logical_overflow).is_err());

        let excessive_depth = raw_leaf(&[], 33);
        assert!(ExtentTree::parse_node(&excessive_depth).is_err());
    }

    #[test]
    fn malformed_inline_root_is_corruption_not_a_hole() {
        let (mut dev, _fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();
        inode.i_block[0] = 0;

        let mut tree = ExtentTree::new(&mut inode, BLOCK_SIZE);
        assert!(tree.find_extent(&mut dev, 0).is_err());
    }

    #[test]
    fn traversal_rejects_child_depth_mismatch_and_physical_range() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let child_block = fs.alloc_block(&mut dev).unwrap();

        let mut malformed_child = [0u8; 60];
        Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 1,
            eh_max: 4,
            eh_depth: 1,
            eh_generation: 0,
        }
        .to_disk_bytes(&mut malformed_child[..Ext4ExtentHeader::disk_size()]);
        Ext4ExtentIdx {
            ei_block: 0,
            ei_leaf_lo: child_block.raw() as u32,
            ei_leaf_hi: (child_block.raw() >> 32) as u16,
            ei_unused: 0,
        }
        .to_disk_bytes(
            &mut malformed_child[Ext4ExtentHeader::disk_size()
                ..Ext4ExtentHeader::disk_size() + Ext4ExtentIdx::disk_size()],
        );
        dev.read_block(child_block).unwrap();
        dev.buffer_mut()[..malformed_child.len()].copy_from_slice(&malformed_child);
        dev.write_block(child_block, false).unwrap();

        let mut inode = new_extent_inode();
        let root = ExtentNode::Index {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 1,
                eh_max: 4,
                eh_depth: 1,
                eh_generation: 0,
            },
            entries: vec![Ext4ExtentIdx {
                ei_block: 0,
                ei_leaf_lo: child_block.raw() as u32,
                ei_leaf_hi: (child_block.raw() >> 32) as u16,
                ei_unused: 0,
            }],
        };
        {
            let mut tree = ExtentTree::new(&mut inode, BLOCK_SIZE);
            tree.store_root_to_inode(&root).unwrap();
        }
        assert!(
            ExtentTree::new(&mut inode, BLOCK_SIZE)
                .find_extent(&mut dev, 0)
                .is_err()
        );

        let mut out_of_range_inode = new_extent_inode();
        let out_of_range = ExtentNode::Leaf {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 1,
                eh_max: 4,
                eh_depth: 0,
                eh_generation: 0,
            },
            entries: vec![Ext4Extent::new(0, dev.total_blocks(), 1)],
        };
        {
            let mut tree = ExtentTree::new(&mut out_of_range_inode, BLOCK_SIZE);
            tree.store_root_to_inode(&out_of_range).unwrap();
        }
        assert!(
            ExtentTree::new(&mut out_of_range_inode, BLOCK_SIZE)
                .find_extent(&mut dev, 0)
                .is_err()
        );
    }

    #[test]
    fn traversal_rejects_extent_into_system_metadata() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let inode_num = InodeNumber::new(12).unwrap();
        let mut inode = new_extent_inode();
        let block_bitmap = fs.group_descs[0].block_bitmap();
        let root = ExtentNode::Leaf {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 1,
                eh_max: 4,
                eh_depth: 0,
                eh_generation: 0,
            },
            entries: vec![Ext4Extent::new(0, block_bitmap, 1)],
        };
        ExtentTree::new(&mut inode, BLOCK_SIZE)
            .store_root_to_inode(&root)
            .unwrap();

        let error = ExtentTree::with_filesystem(&mut inode, &fs, inode_num)
            .find_extent(&mut dev, 0)
            .expect_err("ordinary inode must not map the block bitmap");
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "extent:system_metadata",
            })
        );

        fs.set_block_validity(&mut dev, false).unwrap();
        let extent = ExtentTree::with_filesystem(&mut inode, &fs, inode_num)
            .find_extent(&mut dev, 0)
            .expect("noblock_validity must bypass the system-zone index")
            .expect("crafted extent must remain present");
        assert_eq!(extent.start_block(), block_bitmap);

        fs.set_block_validity(&mut dev, true).unwrap();
        let error = ExtentTree::with_filesystem(&mut inode, &fs, inode_num)
            .find_extent(&mut dev, 0)
            .expect_err("reenabling block validity must rebuild the system-zone index");
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "extent:system_metadata",
            })
        );
    }

    #[test]
    fn journal_inode_alone_may_map_its_protected_blocks() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let journal_ino = InodeNumber::new(fs.superblock.s_journal_inum).unwrap();
        let mut journal_inode = fs.get_inode_by_num(&mut dev, journal_ino).unwrap();
        let journal_first = resolve_inode_block(&fs, &mut dev, journal_ino, &mut journal_inode, 0)
            .unwrap()
            .expect("journal must have a first block");

        let ordinary_ino = InodeNumber::new(12).unwrap();
        let mut ordinary_inode = new_extent_inode();
        let root = ExtentNode::Leaf {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 1,
                eh_max: 4,
                eh_depth: 0,
                eh_generation: 0,
            },
            entries: vec![Ext4Extent::new(0, journal_first.raw(), 1)],
        };
        ExtentTree::new(&mut ordinary_inode, BLOCK_SIZE)
            .store_root_to_inode(&root)
            .unwrap();

        let error = ExtentTree::with_filesystem(&mut ordinary_inode, &fs, ordinary_ino)
            .find_extent(&mut dev, 0)
            .expect_err("ordinary inode must not reuse an internal-journal block");
        assert_eq!(
            error.context(),
            Some(ErrorContext::Operation {
                op: "extent:system_metadata",
            })
        );
    }

    #[test]
    fn inline_root_store_rejects_overcapacity_without_mutating_inode() {
        let mut inode = new_extent_inode();
        let original = inode.i_block;
        let node = ExtentNode::Leaf {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 5,
                eh_max: 5,
                eh_depth: 0,
                eh_generation: 0,
            },
            entries: (0..5)
                .map(|slot| Ext4Extent::new(slot * 2, 100 + u64::from(slot), 1))
                .collect(),
        };

        let error = ExtentTree::new(&mut inode, BLOCK_SIZE)
            .store_root_to_inode(&node)
            .expect_err("five entries cannot fit the inode-inline root");
        assert!(error.is_corruption());
        assert_eq!(inode.i_block, original);
    }

    #[test]
    fn external_extent_node_checksum_is_verified_before_traversal() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let data_block = fs.alloc_block(&mut dev).unwrap();
        let child_block = fs.alloc_block(&mut dev).unwrap();
        let inode_num = InodeNumber::new(12).unwrap();
        let mut inode = new_extent_inode();
        inode.i_generation = 0x1234_5678;

        let child = ExtentNode::Leaf {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 1,
                eh_max: ((BLOCK_SIZE - Ext4ExtentHeader::disk_size() - 4) / Ext4Extent::disk_size())
                    as u16,
                eh_depth: 0,
                eh_generation: 0,
            },
            entries: vec![Ext4Extent::new(0, data_block.raw(), 1)],
        };
        let root = ExtentNode::Index {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 1,
                eh_max: 4,
                eh_depth: 1,
                eh_generation: 0,
            },
            entries: vec![Ext4ExtentIdx {
                ei_block: 0,
                ei_leaf_lo: child_block.raw() as u32,
                ei_leaf_hi: (child_block.raw() >> 32) as u16,
                ei_unused: 0,
            }],
        };

        {
            let mut tree = ExtentTree::with_filesystem(&mut inode, &fs, inode_num);
            tree.write_node_to_block(&mut dev, child_block, &child)
                .unwrap();
            tree.store_root_to_inode(&root).unwrap();
            assert_eq!(
                tree.find_extent(&mut dev, 0)
                    .unwrap()
                    .expect("mapped extent")
                    .start_block(),
                data_block.raw()
            );
        }

        dev.read_block(child_block).unwrap();
        dev.buffer_mut()[BLOCK_SIZE - 1] ^= 0x80;
        dev.write_block(child_block, false).unwrap();

        let error = ExtentTree::with_filesystem(&mut inode, &fs, inode_num)
            .find_extent(&mut dev, 0)
            .expect_err("corrupt external-node checksum must fail before traversal");
        assert_eq!(error.kind(), crate::Ext4ErrorKind::ChecksumMismatch);
    }

    #[test]
    fn extent_runs_clip_single_extent_to_requested_range() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();
        let base = alloc_contiguous(&mut fs, &mut dev, 10);
        {
            let mut tree = ExtentTree::new(&mut inode, BLOCK_SIZE);
            tree.insert_extent(&mut fs, Ext4Extent::new(10, base.raw(), 10), &mut dev)
                .unwrap();
        }

        let mut tree = ExtentTree::new(&mut inode, BLOCK_SIZE);
        let runs = tree.initialized_runs_in_range(&mut dev, 12, 15).unwrap();

        assert_eq!(
            runs,
            [ExtentRun {
                logical_start: 12,
                physical_start: base.checked_add(2).unwrap(),
                len: 4,
            }]
        );
    }

    #[test]
    fn extent_runs_return_only_initialized_overlapping_sparse_runs() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();
        let base1 = alloc_contiguous(&mut fs, &mut dev, 2);
        let base2 = alloc_contiguous(&mut fs, &mut dev, 2);
        {
            let mut tree = ExtentTree::new(&mut inode, BLOCK_SIZE);
            tree.insert_extent(&mut fs, Ext4Extent::new(0, base1.raw(), 2), &mut dev)
                .unwrap();
            tree.insert_extent(&mut fs, Ext4Extent::new(4, base2.raw(), 2), &mut dev)
                .unwrap();
        }

        let mut tree = ExtentTree::new(&mut inode, BLOCK_SIZE);
        let runs = tree.initialized_runs_in_range(&mut dev, 1, 4).unwrap();

        assert_eq!(
            runs,
            [
                ExtentRun {
                    logical_start: 1,
                    physical_start: base1.checked_add(1).unwrap(),
                    len: 1,
                },
                ExtentRun {
                    logical_start: 4,
                    physical_start: base2,
                    len: 1,
                },
            ]
        );
    }

    #[test]
    fn insert_rejects_empty_internal_root_without_mutating_inode() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();
        let empty_root = ExtentNode::Index {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 0,
                eh_max: 4,
                eh_depth: 1,
                eh_generation: 0,
            },
            entries: Vec::new(),
        };
        ExtentTree::new(&mut inode).store_root_to_inode(&empty_root);
        let inode_before = inode.i_block;

        let result =
            ExtentTree::new(&mut inode).insert_extent(&mut fs, Ext4Extent::new(0, 1, 1), &mut dev);

        assert_eq!(result, Err(Ext4Error::corrupted()));
        assert_eq!(inode.i_block, inode_before);
    }

    #[test]
    fn insert_rejects_parent_with_empty_internal_child_without_mutating_metadata() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();
        let child_block = fs.alloc_block(&mut dev).unwrap();
        let empty_child = ExtentNode::Index {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 0,
                eh_max: ExtentTree::calc_block_eh_max(),
                eh_depth: 1,
                eh_generation: 0,
            },
            entries: Vec::new(),
        };
        let root = ExtentNode::Index {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 1,
                eh_max: 4,
                eh_depth: 1,
                eh_generation: 0,
            },
            entries: vec![Ext4ExtentIdx {
                ei_block: 0,
                ei_leaf_lo: child_block.raw() as u32,
                ei_leaf_hi: (child_block.raw() >> 32) as u16,
                ei_unused: 0,
            }],
        };
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.write_node_to_block(&mut dev, child_block, &empty_child)
                .unwrap();
            tree.store_root_to_inode(&root);
        }

        let inode_before = inode.i_block;
        dev.read_block(child_block).unwrap();
        let child_before = dev.buffer().to_vec();

        let result = ExtentTree::new(&mut inode).insert_extent(
            &mut fs,
            Ext4Extent::new(0, child_block.raw(), 1),
            &mut dev,
        );

        assert_eq!(result, Err(Ext4Error::corrupted()));
        assert_eq!(inode.i_block, inode_before);
        dev.read_block(child_block).unwrap();
        assert_eq!(dev.buffer(), child_before);
    }
}
