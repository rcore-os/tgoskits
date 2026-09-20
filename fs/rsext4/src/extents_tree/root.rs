use super::*;
use crate::{
    bmalloc::InodeNumber,
    crc32c::{ext4_crc32c_seed_from_superblock, ext4_superblock_has_metadata_csum},
    ext4::{Ext4FileSystem, SystemZoneMap},
    superblock::Ext4Superblock,
};

/// Extent-tree view bound to a single inode.
pub struct ExtentTree<'a> {
    pub inode: &'a mut Ext4Inode,
    pub(super) block_size: usize,
    inode_num: Option<InodeNumber>,
    generation: u32,
    pub(super) checksum_seed: Option<u32>,
    first_data_block: u64,
    filesystem_blocks: u64,
    system_zones: SystemZoneMap,
}

impl<'a> ExtentTree<'a> {
    /// Creates a geometry-free extent-tree handle for in-crate tests.
    ///
    /// Production paths must use [`Self::with_filesystem`] so physical-range,
    /// system-zone, and metadata-checksum validation carry mounted context.
    #[cfg(test)]
    pub(crate) fn new(inode: &'a mut Ext4Inode, block_size: usize) -> Self {
        let generation = inode.i_generation;
        Self {
            inode,
            block_size,
            inode_num: None,
            generation,
            checksum_seed: None,
            first_data_block: 0,
            filesystem_blocks: u64::MAX,
            system_zones: SystemZoneMap::default(),
        }
    }

    /// Creates a checksum-aware handle without a mounted system-zone index.
    fn with_checksum(
        inode: &'a mut Ext4Inode,
        superblock: &Ext4Superblock,
        inode_num: InodeNumber,
    ) -> Self {
        let generation = inode.i_generation;
        Self {
            inode,
            block_size: superblock.block_size() as usize,
            inode_num: Some(inode_num),
            generation,
            checksum_seed: ext4_superblock_has_metadata_csum(superblock)
                .then(|| ext4_crc32c_seed_from_superblock(superblock)),
            first_data_block: u64::from(superblock.s_first_data_block),
            filesystem_blocks: superblock.blocks_count(),
            system_zones: SystemZoneMap::default(),
        }
    }

    /// Creates an extent-tree handle bound to all mounted validation context.
    pub fn with_filesystem(
        inode: &'a mut Ext4Inode,
        filesystem: &Ext4FileSystem,
        inode_num: InodeNumber,
    ) -> Self {
        let mut tree = Self::with_checksum(inode, &filesystem.superblock, inode_num);
        tree.system_zones = filesystem.system_zones.clone();
        tree
    }

    pub(super) fn bind_geometry(&mut self, superblock: &Ext4Superblock) {
        self.block_size = superblock.block_size() as usize;
        self.first_data_block = u64::from(superblock.s_first_data_block);
        self.filesystem_blocks = superblock.blocks_count();
    }

    fn physical_block_limit(&self, device_blocks: u64) -> u64 {
        self.filesystem_blocks.min(device_blocks)
    }

    pub(super) fn validate_physical_range(
        &self,
        start: u64,
        len: u64,
        device_blocks: u64,
    ) -> Ext4Result<()> {
        let end = start
            .checked_add(len)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:physical_overflow"))?;
        if start <= self.first_data_block || end > self.physical_block_limit(device_blocks) {
            return Err(Ext4Error::corrupted().with_operation("extent:physical_range"));
        }
        if !self.system_zones.is_empty() {
            let inode_num = self
                .inode_num
                .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:missing_inode"))?;
            if !self.system_zones.allows_range(start, len, inode_num) {
                return Err(Ext4Error::corrupted().with_operation("extent:system_metadata"));
            }
        }
        Ok(())
    }

    pub(super) fn validate_node(
        &self,
        node: &ExtentNode,
        expected_depth: Option<u16>,
        parent_key: Option<u32>,
        device_blocks: u64,
        inline_root: bool,
    ) -> Ext4Result<()> {
        let header = node.header();
        if expected_depth.is_some_and(|depth| header.eh_depth != depth) {
            return Err(Ext4Error::corrupted().with_operation("extent:depth_mismatch"));
        }
        if header.eh_depth > Self::MAX_DEPTH || header.eh_magic != Ext4ExtentHeader::EXT4_EXT_MAGIC
        {
            return Err(Ext4Error::corrupted().with_operation("extent:header"));
        }

        let max_entries = if inline_root {
            (self.inode.i_block.len() * core::mem::size_of::<u32>() - Ext4ExtentHeader::disk_size())
                / Ext4Extent::disk_size()
        } else {
            usize::from(self.calc_block_eh_max())
        };
        let actual_entries = match node {
            ExtentNode::Leaf { entries, .. } => entries.len(),
            ExtentNode::Index { entries, .. } => entries.len(),
        };
        if header.eh_max == 0
            || usize::from(header.eh_max) > max_entries
            || usize::from(header.eh_entries) != actual_entries
            || actual_entries > usize::from(header.eh_max)
        {
            return Err(Ext4Error::corrupted().with_operation("extent:node_capacity"));
        }

        match node {
            ExtentNode::Leaf { entries, .. } => {
                Self::validate_leaf_entries(entries)?;
                if let (Some(expected), Some(first)) =
                    (parent_key, entries.first().map(|extent| extent.ee_block))
                    && first != expected
                {
                    return Err(Ext4Error::corrupted().with_operation("extent:parent_key"));
                }
                for extent in entries {
                    self.validate_physical_range(
                        extent.start_block(),
                        u64::from(extent.len()),
                        device_blocks,
                    )?;
                }
            }
            ExtentNode::Index { entries, .. } => {
                if entries.is_empty() {
                    return Err(Ext4Error::corrupted().with_operation("extent:empty_index"));
                }
                Self::validate_index_entries(entries)?;
                if parent_key.is_some_and(|expected| entries[0].ei_block != expected) {
                    return Err(Ext4Error::corrupted().with_operation("extent:parent_key"));
                }
                for index in entries {
                    self.validate_physical_range(
                        ((index.ei_leaf_hi as u64) << 32) | u64::from(index.ei_leaf_lo),
                        1,
                        device_blocks,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn extent_block_checksum_offset(
        bytes_len: usize,
        header: &Ext4ExtentHeader,
    ) -> Ext4Result<usize> {
        let offset = usize::from(header.eh_max)
            .checked_mul(Ext4Extent::disk_size())
            .and_then(|entries| entries.checked_add(Ext4ExtentHeader::disk_size()))
            .ok_or_else(Ext4Error::overflow)?;
        let end = offset
            .checked_add(core::mem::size_of::<u32>())
            .ok_or_else(Ext4Error::overflow)?;
        if end > bytes_len {
            return Err(Ext4Error::corrupted().with_operation("extent:checksum_tail"));
        }
        Ok(offset)
    }

    fn verify_extent_block_checksum(
        &self,
        bytes: &[u8],
        header: &Ext4ExtentHeader,
    ) -> Ext4Result<()> {
        let (Some(seed), Some(inode_num)) = (self.checksum_seed, self.inode_num) else {
            return Ok(());
        };
        let tail = Self::extent_block_checksum_offset(bytes.len(), header)?;
        let stored = u32::from_le_bytes(
            bytes[tail..tail + core::mem::size_of::<u32>()]
                .try_into()
                .map_err(|_| Ext4Error::corrupted().with_operation("extent:checksum_tail"))?,
        );
        let inode_le = inode_num.raw().to_le_bytes();
        let generation_le = self.generation.to_le_bytes();
        let computed = crate::checksum::ext4_metadata_csum32(
            seed,
            &[&inode_le, &generation_le, &bytes[..tail]],
        );
        if stored != computed {
            return Err(Ext4Error::checksum().with_operation("extent:block_checksum"));
        }
        Ok(())
    }

    pub(super) fn read_child_node<B: BlockIo>(
        &self,
        dev: &mut Jbd2Dev<B>,
        index: &Ext4ExtentIdx,
        expected_depth: u16,
    ) -> Ext4Result<ExtentNode> {
        let child_block =
            AbsoluteBN::new(((index.ei_leaf_hi as u64) << 32) | u64::from(index.ei_leaf_lo));
        self.validate_physical_range(child_block.raw(), 1, dev.total_blocks())?;
        dev.read_block(child_block)?;
        let child = Self::parse_node_from_bytes(dev.buffer())?;
        self.validate_node(
            &child,
            Some(expected_depth),
            Some(index.ei_block),
            dev.total_blocks(),
            false,
        )?;
        self.verify_extent_block_checksum(dev.buffer(), child.header())?;
        Ok(child)
    }

    fn huge_file_feature(fs: &Ext4FileSystem) -> bool {
        fs.superblock
            .has_feature_ro_compat(Ext4Superblock::EXT4_FEATURE_RO_COMPAT_HUGE_FILE)
    }

    pub(super) fn can_add_inode_sectors_for_block(&self, fs: &Ext4FileSystem) -> Ext4Result<()> {
        let mut inode = *self.inode;
        let add_sectors = (self.block_size / 512) as u64;
        let huge_file_feature = Self::huge_file_feature(fs);
        let current = inode.blocks_count(self.block_size as u32, huge_file_feature);
        let next = current
            .checked_add(add_sectors)
            .ok_or_else(Ext4Error::overflow)?;
        inode.set_blocks_count(next, self.block_size as u32, huge_file_feature)
    }

    pub(super) fn add_inode_sectors_for_block(&mut self, fs: &Ext4FileSystem) -> Ext4Result<()> {
        let add_sectors = (self.block_size / 512) as u64;
        let huge_file_feature = Self::huge_file_feature(fs);
        let current = self
            .inode
            .blocks_count(self.block_size as u32, huge_file_feature);
        let next = current
            .checked_add(add_sectors)
            .ok_or_else(Ext4Error::overflow)?;
        self.inode
            .set_blocks_count(next, self.block_size as u32, huge_file_feature)
    }

    pub(super) fn sub_inode_sectors_for_block(&mut self, fs: &Ext4FileSystem) -> Ext4Result<()> {
        let sub_sectors = (self.block_size / 512) as u64;
        let huge_file_feature = Self::huge_file_feature(fs);
        let current = self
            .inode
            .blocks_count(self.block_size as u32, huge_file_feature);
        let next = current
            .checked_sub(sub_sectors)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:sub_inode_blocks"))?;
        self.inode
            .set_blocks_count(next, self.block_size as u32, huge_file_feature)
    }

    pub(super) fn can_sub_inode_sectors_for_blocks(
        &self,
        fs: &Ext4FileSystem,
        blocks: u64,
    ) -> Ext4Result<()> {
        let mut inode = *self.inode;
        let huge_file_feature = Self::huge_file_feature(fs);
        let sub_sectors = blocks
            .checked_mul((self.block_size / 512) as u64)
            .ok_or_else(Ext4Error::overflow)?;
        let current = inode.blocks_count(self.block_size as u32, huge_file_feature);
        let next = current
            .checked_sub(sub_sectors)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:sub_inode_blocks"))?;
        inode.set_blocks_count(next, self.block_size as u32, huge_file_feature)
    }

    pub(super) fn inline_eh_max_for_node(&self, node: &ExtentNode) -> u16 {
        let inline_bytes = self.inode.i_block.len() * core::mem::size_of::<u32>();
        let entry_size = match node {
            ExtentNode::Leaf { .. } => Ext4Extent::disk_size(),
            ExtentNode::Index { .. } => Ext4ExtentIdx::disk_size(),
        };
        (inline_bytes.saturating_sub(Ext4ExtentHeader::disk_size()) / entry_size) as u16
    }

    /// Publishes an external root child in the inode before releasing its block.
    ///
    /// The caller owns enough metadata and revoke credits in the current
    /// filesystem transaction. Returning `false` means the child does not fit
    /// and leaves both the inode and allocation state unchanged.
    #[cold]
    #[inline(never)]
    pub(super) fn collapse_external_root_child<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        child_block: AbsoluteBN,
        mut child: ExtentNode,
    ) -> Ext4Result<bool> {
        let inline_max = self.inline_eh_max_for_node(&child);
        let child_entries = match &child {
            ExtentNode::Leaf { entries, .. } => entries.len(),
            ExtentNode::Index { entries, .. } => entries.len(),
        };
        if child_entries > usize::from(inline_max) {
            return Ok(false);
        }

        self.can_sub_inode_sectors_for_blocks(fs, 1)?;
        let inode_before = *self.inode;
        let result = (|| {
            child.header_mut().eh_max = inline_max;
            self.store_root_to_inode(&child)?;
            block_dev.forget_detached_metadata(child_block)?;
            fs.free_block(block_dev, child_block)?;
            self.sub_inode_sectors_for_block(fs)
        })();
        if let Err(error) = result {
            *self.inode = inode_before;
            return Err(error);
        }
        Ok(true)
    }

    /// Walks all extent-tree blocks that live outside the inode's inline root.
    pub fn external_node_blocks<B: BlockIo>(
        &self,
        dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<Vec<AbsoluteBN>> {
        let root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, dev.total_blocks(), true)?;

        fn walk<B: BlockIo>(
            tree: &ExtentTree<'_>,
            dev: &mut Jbd2Dev<B>,
            node: &ExtentNode,
            out: &mut Vec<AbsoluteBN>,
        ) -> Ext4Result<()> {
            match node {
                ExtentNode::Leaf { .. } => Ok(()),
                ExtentNode::Index { header, entries } => {
                    for idx in entries {
                        let child = AbsoluteBN::new(
                            ((idx.ei_leaf_hi as u64) << 32) | idx.ei_leaf_lo as u64,
                        );
                        out.push(child);
                        let child_node = tree.read_child_node(dev, idx, header.eh_depth - 1)?;
                        walk(tree, dev, &child_node, out)?;
                    }
                    Ok(())
                }
            }
        }

        let mut blocks = Vec::new();
        walk(self, dev, &root, &mut blocks)?;
        blocks.sort_unstable();
        blocks.dedup();
        Ok(blocks)
    }

    /// Parses the inline extent root from `inode.i_block`.
    pub fn load_root_from_inode(&self) -> Ext4Result<ExtentNode> {
        // `inode.i_block` holds 15 little-endian words, which is exactly enough
        // for one inline extent node.
        let iblocks = &self.inode.i_block;
        let mut bytes: [u8; 60] = [0; 60];
        for idx in 0..15 {
            // Re-encode each word as little-endian before parsing.
            let trans_b1 = iblocks[idx].to_le_bytes();
            bytes[idx * 4] = trans_b1[0];
            bytes[idx * 4 + 1] = trans_b1[1];
            bytes[idx * 4 + 2] = trans_b1[2];
            bytes[idx * 4 + 3] = trans_b1[3];
        }
        Self::parse_node_from_bytes(&bytes)
    }

    /// Serializes the root node back into `inode.i_block`.
    pub fn store_root_to_inode(&mut self, node: &ExtentNode) -> Ext4Result<()> {
        self.validate_node(node, None, None, self.filesystem_blocks, true)?;
        let hdr_size = Ext4ExtentHeader::disk_size();

        match node {
            ExtentNode::Leaf { header, entries } => {
                // Inline leaf root: header plus extents packed into 60 bytes.
                let mut buf = [0u8; 60];

                header.to_disk_bytes(&mut buf[0..hdr_size]);

                let et_size = Ext4Extent::disk_size();
                for (i, e) in entries.iter().enumerate() {
                    let off = hdr_size + i * et_size;
                    e.to_disk_bytes(&mut buf[off..off + et_size]);
                }

                // Copy the serialized bytes back as 15 little-endian words.
                for (i, block) in self.inode.i_block.iter_mut().enumerate() {
                    let off = i * 4;
                    let v =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    *block = v;
                }
            }
            ExtentNode::Index { header, entries } => {
                // Inline index root: header plus child indexes packed into `i_block`.
                let mut buf = [0u8; 60];

                header.to_disk_bytes(&mut buf[0..hdr_size]);

                let idx_size = Ext4ExtentIdx::disk_size();
                for (i, idx) in entries.iter().enumerate() {
                    let off = hdr_size + i * idx_size;
                    idx.to_disk_bytes(&mut buf[off..off + idx_size]);
                }

                for (i, block) in self.inode.i_block.iter_mut().enumerate() {
                    let off = i * 4;
                    let v =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    *block = v;
                }
            }
        }
        Ok(())
    }

    /// Writes an extent node to an absolute physical block.
    fn update_extent_block_checksum(
        &self,
        buf: &mut [u8],
        header: &Ext4ExtentHeader,
    ) -> Ext4Result<()> {
        let (Some(seed), Some(inode_num)) = (self.checksum_seed, self.inode_num) else {
            return Ok(());
        };

        let tail = Self::extent_block_checksum_offset(buf.len(), header)?;
        let inode_le = inode_num.raw().to_le_bytes();
        let generation_le = self.generation.to_le_bytes();
        let checksum =
            crate::checksum::ext4_metadata_csum32(seed, &[&inode_le, &generation_le, &buf[..tail]]);
        buf[tail..tail + core::mem::size_of::<u32>()].copy_from_slice(&checksum.to_le_bytes());
        Ok(())
    }

    pub(super) fn write_node_to_block<B: BlockIo>(
        &self,
        dev: &mut Jbd2Dev<B>,
        block_id: AbsoluteBN,
        node: &ExtentNode,
    ) -> Ext4Result<()> {
        let hdr_size = Ext4ExtentHeader::disk_size();
        let block_eh_max = self.calc_block_eh_max();
        let entry_count = match node {
            ExtentNode::Leaf { entries, .. } => entries.len(),
            ExtentNode::Index { entries, .. } => entries.len(),
        };
        if entry_count > usize::from(block_eh_max) {
            return Err(Ext4Error::corrupted().with_operation("extent:node_capacity"));
        }
        self.validate_node(node, None, None, dev.total_blocks(), false)?;
        dev.update_block(block_id, true, |buf| {
            buf.fill(0);

            match node {
                ExtentNode::Leaf { header, entries } => {
                    let et_size = Ext4Extent::disk_size();
                    let mut disk_header = *header;
                    disk_header.eh_max = block_eh_max;
                    disk_header.to_disk_bytes(&mut buf[0..hdr_size]);
                    for (i, e) in entries.iter().enumerate() {
                        let off = hdr_size + i * et_size;
                        if off + et_size > buf.len() {
                            break;
                        }
                        e.to_disk_bytes(&mut buf[off..off + et_size]);
                    }
                }
                ExtentNode::Index { header, entries } => {
                    let idx_size = Ext4ExtentIdx::disk_size();
                    let mut disk_header = *header;
                    disk_header.eh_max = block_eh_max;

                    disk_header.to_disk_bytes(&mut buf[0..hdr_size]);
                    for (i, idx) in entries.iter().enumerate() {
                        let off = hdr_size + i * idx_size;
                        if off + idx_size > buf.len() {
                            break;
                        }
                        idx.to_disk_bytes(&mut buf[off..off + idx_size]);
                    }
                }
            }
            let disk_header = Ext4ExtentHeader::from_disk_bytes(&buf[..hdr_size]);
            self.update_extent_block_checksum(buf, &disk_header)
        })
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::endian::DiskFormat;

    #[test]
    fn extent_checksum_tail_follows_eh_max_for_two_kib_nodes() {
        let superblock = Ext4Superblock {
            s_log_block_size: 1,
            s_blocks_count_lo: 1024,
            s_feature_ro_compat: Ext4Superblock::EXT4_FEATURE_RO_COMPAT_METADATA_CSUM,
            s_uuid: [0x42; 16],
            ..Ext4Superblock::default()
        };
        let generation = 0x1234_5678;
        let mut inode = Ext4Inode {
            i_generation: generation,
            ..Default::default()
        };
        let inode_num = InodeNumber::new(12).unwrap();
        let tree = ExtentTree::with_checksum(&mut inode, &superblock, inode_num);

        let mut block = vec![0u8; 2048];
        let header = Ext4ExtentHeader {
            eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
            eh_entries: 0,
            eh_max: ((block.len() - Ext4ExtentHeader::disk_size()) / Ext4Extent::disk_size())
                as u16,
            eh_depth: 0,
            eh_generation: 0,
        };
        header.to_disk_bytes(&mut block[..Ext4ExtentHeader::disk_size()]);
        tree.update_extent_block_checksum(&mut block, &header)
            .unwrap();

        let tail = ExtentTree::extent_block_checksum_offset(block.len(), &header).unwrap();
        assert_eq!(tail, 2040);
        let stored = u32::from_le_bytes(block[tail..tail + 4].try_into().unwrap());
        let expected = crate::checksum::ext4_metadata_csum32(
            ext4_crc32c_seed_from_superblock(&superblock),
            &[
                &inode_num.raw().to_le_bytes(),
                &generation.to_le_bytes(),
                &block[..tail],
            ],
        );
        assert_eq!(stored, expected);
        assert_eq!(&block[tail + 4..], &[0; 4]);
    }
}
