use super::*;
use crate::{
    bmalloc::InodeNumber,
    crc32c::{ext4_crc32c_seed_from_superblock, ext4_superblock_has_metadata_csum},
    superblock::Ext4Superblock,
};

/// Extent-tree view bound to a single inode.
pub struct ExtentTree<'a> {
    pub inode: &'a mut Ext4Inode,
    inode_num: Option<InodeNumber>,
    generation: u32,
    checksum_seed: Option<u32>,
}

impl<'a> ExtentTree<'a> {
    /// Creates an extent-tree handle backed by the given inode.
    pub fn new(inode: &'a mut Ext4Inode) -> Self {
        let generation = inode.i_generation;
        Self {
            inode,
            inode_num: None,
            generation,
            checksum_seed: None,
        }
    }

    /// Creates an extent-tree handle with enough metadata to checksum external nodes.
    pub fn with_checksum(
        inode: &'a mut Ext4Inode,
        superblock: &Ext4Superblock,
        inode_num: InodeNumber,
    ) -> Self {
        let generation = inode.i_generation;
        Self {
            inode,
            inode_num: Some(inode_num),
            generation,
            checksum_seed: ext4_superblock_has_metadata_csum(superblock)
                .then(|| ext4_crc32c_seed_from_superblock(superblock)),
        }
    }

    pub(super) fn add_inode_sectors_for_block(&mut self) {
        let add_sectors = (BLOCK_SIZE / 512) as u64;
        let cur = ((self.inode.l_i_blocks_high as u64) << 32) | (self.inode.i_blocks_lo as u64);
        let newv = cur.saturating_add(add_sectors);
        self.inode.i_blocks_lo = (newv & 0xFFFF_FFFF) as u32;
        self.inode.l_i_blocks_high = ((newv >> 32) & 0xFFFF) as u16;
    }

    pub(super) fn sub_inode_sectors_for_block(&mut self) {
        let sub_sectors = (BLOCK_SIZE / 512) as u64;
        let cur = ((self.inode.l_i_blocks_high as u64) << 32) | (self.inode.i_blocks_lo as u64);
        let newv = cur.saturating_sub(sub_sectors);
        self.inode.i_blocks_lo = (newv & 0xFFFF_FFFF) as u32;
        self.inode.l_i_blocks_high = ((newv >> 32) & 0xFFFF) as u16;
    }

    /// Walks all extent-tree blocks that live outside the inode's inline root.
    pub fn external_node_blocks<B: BlockDevice>(
        &self,
        dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<Vec<AbsoluteBN>> {
        let Some(root) = self.load_root_from_inode() else {
            return Ok(Vec::new());
        };

        fn walk<B: BlockDevice>(
            dev: &mut Jbd2Dev<B>,
            node: &ExtentNode,
            out: &mut Vec<AbsoluteBN>,
        ) -> Ext4Result<()> {
            match node {
                ExtentNode::Leaf { .. } => Ok(()),
                ExtentNode::Index { entries, .. } => {
                    for idx in entries {
                        let child = AbsoluteBN::new(
                            ((idx.ei_leaf_hi as u64) << 32) | idx.ei_leaf_lo as u64,
                        );
                        out.push(child);
                        dev.read_block(child)?;
                        let child_node =
                            ExtentTree::parse_node(dev.buffer()).ok_or(Ext4Error::corrupted())?;
                        walk(dev, &child_node, out)?;
                    }
                    Ok(())
                }
            }
        }

        let mut blocks = Vec::new();
        walk(dev, &root, &mut blocks)?;
        blocks.sort_unstable();
        blocks.dedup();
        Ok(blocks)
    }

    /// Parses the inline extent root from `inode.i_block`.
    pub fn load_root_from_inode(&self) -> Option<ExtentNode> {
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
    pub fn store_root_to_inode(&mut self, node: &ExtentNode) {
        let hdr_size = Ext4ExtentHeader::disk_size();

        match node {
            ExtentNode::Leaf { header, entries } => {
                // Inline leaf root: header plus extents packed into 60 bytes.
                let mut buf = [0u8; 60];

                header.to_disk_bytes(&mut buf[0..hdr_size]);

                let et_size = Ext4Extent::disk_size();
                for (i, e) in entries.iter().enumerate() {
                    let off = hdr_size + i * et_size;
                    if off + et_size > buf.len() {
                        break;
                    }
                    e.to_disk_bytes(&mut buf[off..off + et_size]);
                }

                // Copy the serialized bytes back as 15 little-endian words.
                for i in 0..15 {
                    let off = i * 4;
                    let v =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    self.inode.i_block[i] = v;
                }
            }
            ExtentNode::Index { header, entries } => {
                // Inline index root: header plus child indexes packed into `i_block`.
                let mut buf = [0u8; 60];

                header.to_disk_bytes(&mut buf[0..hdr_size]);

                let idx_size = Ext4ExtentIdx::disk_size();
                for (i, idx) in entries.iter().enumerate() {
                    let off = hdr_size + i * idx_size;
                    if off + idx_size > buf.len() {
                        break;
                    }
                    idx.to_disk_bytes(&mut buf[off..off + idx_size]);
                }

                for i in 0..15 {
                    let off = i * 4;
                    let v =
                        u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]);
                    self.inode.i_block[i] = v;
                }
            }
        }
    }

    /// Writes an extent node to an absolute physical block.
    fn update_extent_block_checksum(&self, buf: &mut [u8]) {
        let (Some(seed), Some(inode_num)) = (self.checksum_seed, self.inode_num) else {
            return;
        };
        if buf.len() < 4 {
            return;
        }

        let tail = buf.len() - 4;
        buf[tail..].fill(0);
        let inode_le = inode_num.raw().to_le_bytes();
        let generation_le = self.generation.to_le_bytes();
        let checksum =
            crate::checksum::ext4_metadata_csum32(seed, &[&inode_le, &generation_le, &buf[..tail]]);
        buf[tail..].copy_from_slice(&checksum.to_le_bytes());
    }

    pub(super) fn write_node_to_block<B: BlockDevice>(
        &self,
        dev: &mut Jbd2Dev<B>,
        block_id: AbsoluteBN,
        node: &ExtentNode,
    ) -> Ext4Result<()> {
        let hdr_size = Ext4ExtentHeader::disk_size();
        let block_eh_max = Self::calc_block_eh_max();
        // Load the target block before overwriting the node payload.
        dev.read_block(block_id)?;
        let buf = dev.buffer_mut();
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
        self.update_extent_block_checksum(buf);
        // Mark the metadata block dirty and write it back.
        dev.write_block(block_id, true)?;
        Ok(())
    }
}
