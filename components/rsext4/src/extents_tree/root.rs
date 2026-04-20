use super::*;

/// Extent-tree view bound to a single inode.
pub struct ExtentTree<'a> {
    pub inode: &'a mut Ext4Inode,
}

impl<'a> ExtentTree<'a> {
    /// Creates an extent-tree handle backed by the given inode.
    pub fn new(inode: &'a mut Ext4Inode) -> Self {
        Self { inode }
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
    pub(super) fn write_node_to_block<B: BlockDevice>(
        dev: &mut Jbd2Dev<B>,
        block_id: AbsoluteBN,
        node: &ExtentNode,
        _eh_max: u16,
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
        // Mark the metadata block dirty and write it back.
        dev.write_block(block_id, true)?;
        Ok(())
    }
}
