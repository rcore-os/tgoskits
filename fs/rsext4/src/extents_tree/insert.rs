use super::{split::SplitInfo, *};

impl<'a> ExtentTree<'a> {
    /// Inserts a new extent into the inode's extent tree.
    pub fn insert_extent<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        new_ext: Ext4Extent,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        let mut root = match self.load_root_from_inode() {
            Some(node) => node,
            None => return Err(Ext4Error::corrupted()),
        };

        // Insert into the current root. If the root splits, rebuild a new
        // index root inside the inode.
        let split_result = self.insert_recursive(fs, block_dev, &mut root, new_ext, None)?;

        match split_result {
            None => {
                self.store_root_to_inode(&root);
                Ok(())
            }
            Some(split_info) => {
                // Root split: promote the old inline root into a real block and
                // rebuild the inode root as an index node.
                let new_left_block = fs.alloc_block(block_dev)?;
                self.add_inode_sectors_for_block();

                // Persist the old root contents into the new left child block.
                self.write_node_to_block(block_dev, new_left_block, &root)?;

                // Rebuild the inline root as a two-entry index node.
                let inline_bytes = self.inode.i_block.len() * 4;
                let hdr_size = Ext4ExtentHeader::disk_size();
                let idx_size = Ext4ExtentIdx::disk_size();
                let root_eh_max = (inline_bytes.saturating_sub(hdr_size) / idx_size) as u16;

                let mut new_root_header = Ext4ExtentHeader::new();
                new_root_header.eh_magic = Ext4ExtentHeader::EXT4_EXT_MAGIC;
                new_root_header.eh_depth = root.header().eh_depth + 1;
                new_root_header.eh_entries = 2;
                new_root_header.eh_max = root_eh_max;

                let left_idx = Ext4ExtentIdx {
                    ei_block: Self::get_node_start_block(&root),
                    ei_leaf_lo: (new_left_block.raw() & 0xFFFF_FFFF) as u32,
                    ei_leaf_hi: ((new_left_block.raw() >> 32) & 0xFFFF) as u16,
                    ei_unused: 0,
                };

                // Right child comes from the recursive split result.
                let right_idx = Ext4ExtentIdx {
                    ei_block: split_info.start_block,
                    ei_leaf_lo: (split_info.phy_block.raw() & 0xFFFF_FFFF) as u32,
                    ei_leaf_hi: ((split_info.phy_block.raw() >> 32) & 0xFFFF) as u16,
                    ei_unused: 0,
                };

                let new_root_node = ExtentNode::Index {
                    header: new_root_header,
                    entries: vec![left_idx, right_idx],
                };

                self.store_root_to_inode(&new_root_node);
                Ok(())
            }
        }
    }

    /// Recursive insert worker.
    ///
    /// `phy_block == None` means the current node is the inline inode root.
    fn insert_recursive<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        node: &mut ExtentNode,
        new_ext: Ext4Extent,
        phy_block: Option<AbsoluteBN>,
    ) -> Ext4Result<Option<SplitInfo>> {
        match node {
            ExtentNode::Leaf { header, entries } => {
                let pos = entries
                    .binary_search_by_key(&new_ext.ee_block, |e| e.ee_block)
                    .unwrap_or_else(|i| i);

                if pos > 0 {
                    let prev = &mut entries[pos - 1];

                    let prev_logical = prev.ee_block;
                    let prev_len = prev.len();
                    let new_logical = new_ext.ee_block;
                    let new_len = new_ext.len();

                    if prev_len != 0
                        && new_len != 0
                        && prev.is_unwritten() == new_ext.is_unwritten()
                    {
                        let prev_end = prev_logical.saturating_add(prev_len);

                        if new_logical == prev_end {
                            let prev_phys_start =
                                ((prev.ee_start_hi as u64) << 32) | prev.ee_start_lo as u64;
                            let new_phys_start =
                                ((new_ext.ee_start_hi as u64) << 32) | new_ext.ee_start_lo as u64;

                            if new_phys_start == prev_phys_start + prev_len as u64 {
                                let total = prev_len + new_len;
                                let max_len = if prev.is_unwritten() {
                                    Ext4Extent::EXT_UNINIT_MAX_LEN as u32
                                } else {
                                    Ext4Extent::EXT_INIT_MAX_LEN as u32
                                };

                                if total <= max_len {
                                    prev.ee_len =
                                        prev.build_len_like(total).ok_or(Ext4Error::corrupted())?;

                                    if entries.len() <= header.eh_max as usize {
                                        if let Some(block_id) = phy_block {
                                            // Persist the updated leaf if it is
                                            // already backed by a real block.
                                            let disk_node = ExtentNode::Leaf {
                                                header: *header,
                                                entries: entries.clone(),
                                            };
                                            self.write_node_to_block(
                                                block_dev, block_id, &disk_node,
                                            )?;
                                        }
                                        return Ok(None);
                                    }
                                } else {
                                    prev.ee_len = prev
                                        .build_len_like(max_len)
                                        .ok_or(Ext4Error::corrupted())?;

                                    let remain = total - max_len;
                                    if remain > 0 {
                                        let tail_logical = prev_logical + max_len;
                                        let tail_phys = prev_phys_start + max_len as u64;

                                        let tail = Ext4Extent {
                                            ee_block: tail_logical,
                                            ee_len: new_ext
                                                .build_len_like(remain)
                                                .ok_or(Ext4Error::corrupted())?,
                                            ee_start_hi: (tail_phys >> 32) as u16,
                                            ee_start_lo: (tail_phys & 0xFFFF_FFFF) as u32,
                                        };

                                        let insert_pos = pos;
                                        entries.insert(insert_pos, tail);
                                        header.eh_entries = entries.len() as u16;

                                        if entries.len() <= header.eh_max as usize {
                                            if let Some(block_id) = phy_block {
                                                let disk_node = ExtentNode::Leaf {
                                                    header: *header,
                                                    entries: entries.clone(),
                                                };
                                                self.write_node_to_block(
                                                    block_dev, block_id, &disk_node,
                                                )?;
                                            }
                                            return Ok(None);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                entries.insert(pos, new_ext);
                header.eh_entries = entries.len() as u16;

                // If the leaf still fits, write it back and stop bubbling.
                if entries.len() <= header.eh_max as usize {
                    if let Some(block_id) = phy_block {
                        let disk_node = ExtentNode::Leaf {
                            header: *header,
                            entries: entries.clone(),
                        };
                        self.write_node_to_block(block_dev, block_id, &disk_node)?;
                    }
                    return Ok(None);
                }

                // Split the sorted extents into left and right halves.
                let split_idx = entries.len() / 2;
                let right_entries = entries.split_off(split_idx);
                header.eh_entries = entries.len() as u16;

                // Allocate a new metadata block for the right half.
                let new_phy_block = fs.alloc_block(block_dev)?;
                self.add_inode_sectors_for_block();

                let right_header = Ext4ExtentHeader {
                    eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                    eh_entries: right_entries.len() as u16,
                    eh_max: Self::calc_block_eh_max(),
                    eh_depth: 0,
                    eh_generation: 0,
                };
                let right_node = ExtentNode::Leaf {
                    header: right_header,
                    entries: right_entries,
                };

                // Persist the new right node first.
                self.write_node_to_block(block_dev, new_phy_block, &right_node)?;
                // Then persist the updated left node when it already lives in a
                // real metadata block.
                if let Some(block_id) = phy_block {
                    let disk_node = ExtentNode::Leaf {
                        header: *header,
                        entries: entries.clone(),
                    };
                    self.write_node_to_block(block_dev, block_id, &disk_node)?;
                }

                // Bubble the right node's first logical block and physical block
                // up to the parent.
                let split_key = match &right_node {
                    ExtentNode::Leaf { entries, .. } => entries[0].ee_block,
                    _ => unreachable!(),
                };

                Ok(Some(SplitInfo {
                    start_block: split_key,
                    phy_block: new_phy_block,
                }))
            }

            ExtentNode::Index { header, entries } => {
                // Internal nodes must always have a child to descend into.
                if entries.is_empty() {
                    return Err(Ext4Error::corrupted());
                }

                // Descend through the last child whose key is <= the new extent.
                let pp = entries.partition_point(|idx| idx.ei_block <= new_ext.ee_block);
                let idx_pos = if pp == 0 { 0 } else { pp - 1 };

                let child_phy_block = AbsoluteBN::new(
                    ((entries[idx_pos].ei_leaf_hi as u64) << 32)
                        | (entries[idx_pos].ei_leaf_lo as u64),
                );
                block_dev.read_block(child_phy_block)?;
                let child_bytes = block_dev.buffer();
                let mut child_node =
                    Self::parse_node_from_bytes(child_bytes).ok_or(Ext4Error::corrupted())?;

                let child_split_res = self.insert_recursive(
                    fs,
                    block_dev,
                    &mut child_node,
                    new_ext,
                    Some(child_phy_block),
                )?;

                let new_child_key = Self::get_node_start_block(&child_node);
                if entries[idx_pos].ei_block != new_child_key {
                    entries[idx_pos].ei_block = new_child_key;
                }

                if let Some(split_info) = child_split_res {
                    // Insert the promoted child pointer in sorted order.
                    let new_idx = Ext4ExtentIdx {
                        ei_block: split_info.start_block,
                        ei_leaf_lo: (split_info.phy_block.raw() & 0xFFFF_FFFF) as u32,
                        ei_leaf_hi: ((split_info.phy_block.raw() >> 32) & 0xFFFF) as u16,
                        ei_unused: 0,
                    };

                    let insert_pos = entries
                        .binary_search_by_key(&new_idx.ei_block, |e| e.ei_block)
                        .unwrap_or_else(|i| i);
                    entries.insert(insert_pos, new_idx);
                    header.eh_entries = entries.len() as u16;

                    // Stop here if the index node still fits.
                    if entries.len() <= header.eh_max as usize {
                        if let Some(block_id) = phy_block {
                            let disk_node = ExtentNode::Index {
                                header: *header,
                                entries: entries.clone(),
                            };
                            self.write_node_to_block(block_dev, block_id, &disk_node)?;
                        }
                        return Ok(None);
                    }

                    // Split the sorted child pointers in half.
                    let split_idx = entries.len() / 2;
                    let right_entries = entries.split_off(split_idx);
                    header.eh_entries = entries.len() as u16;

                    // Allocate a block for the new right-hand index node.
                    let new_phy_block = fs.alloc_block(block_dev)?;
                    self.add_inode_sectors_for_block();

                    let right_header = Ext4ExtentHeader {
                        eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                        eh_entries: right_entries.len() as u16,
                        eh_max: Self::calc_block_eh_max(),
                        eh_depth: header.eh_depth,
                        eh_generation: 0,
                    };

                    let right_node = ExtentNode::Index {
                        header: right_header,
                        entries: right_entries,
                    };

                    self.write_node_to_block(block_dev, new_phy_block, &right_node)?;
                    if let Some(block_id) = phy_block {
                        let disk_node = ExtentNode::Index {
                            header: *header,
                            entries: entries.clone(),
                        };
                        self.write_node_to_block(block_dev, block_id, &disk_node)?;
                    }

                    // Bubble the new right child up to the parent.
                    let split_key = match &right_node {
                        ExtentNode::Index { entries, .. } => entries[0].ei_block,
                        _ => unreachable!(),
                    };

                    Ok(Some(SplitInfo {
                        start_block: split_key,
                        phy_block: new_phy_block,
                    }))
                } else {
                    Ok(None)
                }
            }
        }
    }
}
