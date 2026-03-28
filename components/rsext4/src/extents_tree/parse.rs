use super::*;

impl<'a> ExtentTree<'a> {
    pub fn parse_node(bytes: &[u8]) -> Option<ExtentNode> {
        Self::parse_node_from_bytes(bytes)
    }

    /// Parses one extent-tree node from raw bytes.
    pub(super) fn parse_node_from_bytes(bytes: &[u8]) -> Option<ExtentNode> {
        let hdr_size = Ext4ExtentHeader::disk_size();
        if bytes.len() < hdr_size {
            error!(
                "Extent node buffer too small: {} < {}",
                bytes.len(),
                hdr_size
            );
            return None;
        }

        let header = Ext4ExtentHeader::from_disk_bytes(&bytes[..hdr_size]);
        if header.eh_magic != Ext4ExtentHeader::EXT4_EXT_MAGIC {
            error!(
                "Invalid extent header magic: {:x} (expect {:x})",
                header.eh_magic,
                Ext4ExtentHeader::EXT4_EXT_MAGIC
            );
            return None;
        }

        let entries = header.eh_entries as usize;
        let max = header.eh_max as usize;
        if entries > max {
            error!("Extent header entries overflow: entries={entries}, max={max}");
            return None;
        }

        let mut offset = hdr_size;

        if header.eh_depth == 0 {
            // Leaf nodes store extents directly.
            let mut vec = Vec::with_capacity(entries);
            let et_size = Ext4Extent::disk_size();
            for _ in 0..entries {
                if offset + et_size > bytes.len() {
                    error!(
                        "Extent leaf truncated: need {} bytes, have {}",
                        offset + et_size,
                        bytes.len()
                    );
                    return None;
                }
                let et = Ext4Extent::from_disk_bytes(&bytes[offset..offset + et_size]);
                vec.push(et);
                offset += et_size;
            }
            vec.sort_unstable_by_key(|entries| entries.ee_block);
            Some(ExtentNode::Leaf {
                header,
                entries: vec,
            })
        } else {
            // Internal nodes store child indexes.
            let mut vec = Vec::with_capacity(entries);
            let idx_size = Ext4ExtentIdx::disk_size();
            for _ in 0..entries {
                if offset + idx_size > bytes.len() {
                    error!(
                        "Extent index truncated: need {} bytes, have {}",
                        offset + idx_size,
                        bytes.len()
                    );
                    return None;
                }
                let idx = Ext4ExtentIdx::from_disk_bytes(&bytes[offset..offset + idx_size]);
                vec.push(idx);
                offset += idx_size;
            }
            vec.sort_unstable_by_key(|entries| entries.ei_block);
            Some(ExtentNode::Index {
                header,
                entries: vec,
            })
        }
    }

    /// Finds the extent covering `lblock`, if any.
    pub fn find_extent<B: BlockDevice>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        lblock: u32,
    ) -> Ext4Result<Option<Ext4Extent>> {
        let root = match self.load_root_from_inode() {
            Some(node) => node,
            None => return Ok(None),
        };
        self.find_in_node(dev, &root, lblock)
    }

    /// Recursively searches one node for the extent covering `lblock`.
    #[allow(clippy::only_used_in_recursion)]
    fn find_in_node<B: BlockDevice>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        lblock: u32,
    ) -> Ext4Result<Option<Ext4Extent>> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for et in entries {
                    let start = et.ee_block;
                    let len = et.ee_len as u32;
                    let end = start.saturating_add(len); // half-open range [start, end)
                    if lblock >= start && lblock < end {
                        return Ok(Some(*et));
                    }
                }
                Ok(None)
            }
            ExtentNode::Index { entries, .. } => {
                if entries.is_empty() {
                    return Ok(None);
                }

                // Descend through the last child whose key is <= target.
                let mut chosen = &entries[0];
                for idx in entries {
                    if idx.ei_block <= lblock {
                        chosen = idx;
                    } else {
                        break;
                    }
                }

                let child_block =
                    AbsoluteBN::new((chosen.ei_leaf_hi as u64) << 32 | (chosen.ei_leaf_lo as u64));

                debug!("Descending into extent child block {child_block} for lblock {lblock}");

                dev.read_block(child_block)?;
                let buf = dev.buffer();
                let child = match Self::parse_node_from_bytes(buf) {
                    Some(n) => n,
                    None => return Ok(None),
                };

                self.find_in_node(dev, &child, lblock)
            }
        }
    }
}
