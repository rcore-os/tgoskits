use super::{split::SplitInfo, *};
use crate::blockdev::{TransactionCredits, TransactionHandleExtension};

impl<'a> ExtentTree<'a> {
    /// Inserts a new extent into the inode's extent tree.
    pub fn insert_extent<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        new_ext: Ext4Extent,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        self.bind_geometry(&fs.superblock);
        Self::validate_leaf_entries(core::slice::from_ref(&new_ext))?;
        self.validate_physical_range(
            new_ext.start_block(),
            u64::from(new_ext.len()),
            block_dev.total_blocks(),
        )?;
        let mut root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, block_dev.total_blocks(), true)?;

        // Insert into the current root. If the root splits, rebuild a new
        // index root inside the inode.
        let split_result = self.insert_recursive(fs, block_dev, &mut root, new_ext, None)?;

        match split_result {
            None => {
                let may_collapse = matches!(
                    &root,
                    ExtentNode::Index { header, entries }
                        if header.eh_depth == 1 && entries.len() == 1
                );
                if may_collapse && self.try_collapse_single_leaf_root(fs, block_dev, &root)? {
                    Ok(())
                } else {
                    self.store_root_to_inode(&root)
                }
            }
            Some(split_info) => {
                // Root split: promote the old inline root into a real block and
                // rebuild the inode root as an index node.
                self.can_add_inode_sectors_for_block(fs)?;
                let new_left_block = fs.alloc_block(block_dev)?;
                self.add_inode_sectors_for_block(fs)?;

                // Persist the old root contents into the new left child block.
                self.write_node_to_block(block_dev, new_left_block, &root)?;

                // Rebuild the inline root as a two-entry index node.
                let inline_bytes = self.inode.i_block.len() * 4;
                let hdr_size = Ext4ExtentHeader::disk_size();
                let idx_size = Ext4ExtentIdx::disk_size();
                let root_eh_max = (inline_bytes.saturating_sub(hdr_size) / idx_size) as u16;

                let mut new_root_header = Ext4ExtentHeader::new();
                new_root_header.eh_magic = Ext4ExtentHeader::EXT4_EXT_MAGIC;
                new_root_header.eh_depth = root
                    .header()
                    .eh_depth
                    .checked_add(1)
                    .filter(|depth| *depth <= Self::MAX_DEPTH)
                    .ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("extent:depth_overflow")
                    })?;
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

                self.store_root_to_inode(&new_root_node)
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn try_collapse_single_leaf_root<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        root: &ExtentNode,
    ) -> Ext4Result<bool> {
        let ExtentNode::Index { header, entries } = root else {
            return Ok(false);
        };
        if header.eh_depth != 1 || entries.len() != 1 {
            return Ok(false);
        }

        let child_block = AbsoluteBN::new(
            (u64::from(entries[0].ei_leaf_hi) << 32) | u64::from(entries[0].ei_leaf_lo),
        );
        let child = self.read_child_node(block_dev, &entries[0], 0)?;
        let ExtentNode::Leaf { entries, .. } = &child else {
            return Err(Ext4Error::corrupted().with_operation("extent:merge_up_child"));
        };
        if entries.len() > usize::from(self.inline_eh_max_for_node(&child)) {
            return Ok(false);
        }

        let extension = block_dev
            .extend_active_transaction_credits(TransactionCredits::metadata_with_revokes(2, 1))?;
        if !matches!(extension, Some(TransactionHandleExtension::Extended)) {
            return Ok(false);
        }

        self.collapse_external_root_child(fs, block_dev, child_block, child)
    }

    fn merged_leaf_extent_len(left: Ext4Extent, right: Ext4Extent) -> Ext4Result<Option<u16>> {
        if left.is_unwritten() != right.is_unwritten() {
            return Ok(None);
        }

        let left_len = left.len();
        let right_len = right.len();
        let Some(logical_end) = left.ee_block.checked_add(left_len) else {
            return Ok(None);
        };
        if logical_end != right.ee_block {
            return Ok(None);
        }

        let Some(physical_end) = left.start_block().checked_add(u64::from(left_len)) else {
            return Ok(None);
        };
        if physical_end != right.start_block() {
            return Ok(None);
        }

        let Some(merged_len) = left_len.checked_add(right_len) else {
            return Ok(None);
        };
        let max_len = if left.is_unwritten() {
            u32::from(Ext4Extent::EXT_UNINIT_MAX_LEN)
        } else {
            u32::from(Ext4Extent::EXT_INIT_MAX_LEN)
        };
        if merged_len > max_len {
            return Ok(None);
        }

        left.build_len_like(merged_len)
            .map(Some)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:merge_length"))
    }

    fn merge_leaf_extent_right(
        entries: &mut Vec<Ext4Extent>,
        left_index: usize,
    ) -> Ext4Result<bool> {
        let Some(right_index) = left_index
            .checked_add(1)
            .filter(|index| *index < entries.len())
        else {
            return Ok(false);
        };
        let Some(merged_len) =
            Self::merged_leaf_extent_len(entries[left_index], entries[right_index])?
        else {
            return Ok(false);
        };

        entries[left_index].ee_len = merged_len;
        entries.remove(right_index);
        Ok(true)
    }

    pub(super) fn merge_leaf_extent_neighbors(
        entries: &mut Vec<Ext4Extent>,
        extent_index: usize,
    ) -> Ext4Result<()> {
        let merge_index =
            if extent_index > 0 && Self::merge_leaf_extent_right(entries, extent_index - 1)? {
                extent_index - 1
            } else {
                extent_index
            };
        while Self::merge_leaf_extent_right(entries, merge_index)? {}
        Ok(())
    }

    fn insert_and_merge_leaf_extent(
        entries: &mut Vec<Ext4Extent>,
        inserted_index: usize,
        new_extent: Ext4Extent,
    ) -> Ext4Result<()> {
        let merge_index = if inserted_index > 0 {
            let left_index = inserted_index - 1;
            if let Some(merged_len) = Self::merged_leaf_extent_len(entries[left_index], new_extent)?
            {
                // Sequential append is the common path. Merge it in place so
                // normalization does not insert and immediately remove a tail.
                entries[left_index].ee_len = merged_len;
                left_index
            } else {
                entries.insert(inserted_index, new_extent);
                inserted_index
            }
        } else {
            entries.insert(inserted_index, new_extent);
            inserted_index
        };

        while Self::merge_leaf_extent_right(entries, merge_index)? {}
        Ok(())
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
                Self::insert_and_merge_leaf_extent(entries, pos, new_ext)?;
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
                self.can_add_inode_sectors_for_block(fs)?;
                let new_phy_block = fs.alloc_block(block_dev)?;
                self.add_inode_sectors_for_block(fs)?;

                let right_header = Ext4ExtentHeader {
                    eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                    eh_entries: right_entries.len() as u16,
                    eh_max: self.calc_block_eh_max(),
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
                    ExtentNode::Leaf { entries, .. } => entries
                        .first()
                        .map(|extent| extent.ee_block)
                        .ok_or_else(|| {
                            Ext4Error::corrupted().with_operation("extent:empty_split")
                        })?,
                    ExtentNode::Index { .. } => {
                        return Err(Ext4Error::corrupted().with_operation("extent:split_kind"));
                    }
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
                let child_index = entries
                    .get(idx_pos)
                    .copied()
                    .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:empty_index"))?;

                let child_phy_block = AbsoluteBN::new(
                    ((child_index.ei_leaf_hi as u64) << 32) | u64::from(child_index.ei_leaf_lo),
                );
                let mut child_node =
                    self.read_child_node(block_dev, &child_index, header.eh_depth - 1)?;

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
                    self.can_add_inode_sectors_for_block(fs)?;
                    let new_phy_block = fs.alloc_block(block_dev)?;
                    self.add_inode_sectors_for_block(fs)?;

                    let right_header = Ext4ExtentHeader {
                        eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                        eh_entries: right_entries.len() as u16,
                        eh_max: self.calc_block_eh_max(),
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
                        ExtentNode::Index { entries, .. } => {
                            entries.first().map(|index| index.ei_block).ok_or_else(|| {
                                Ext4Error::corrupted().with_operation("extent:empty_split")
                            })?
                        }
                        ExtentNode::Leaf { .. } => {
                            return Err(Ext4Error::corrupted().with_operation("extent:split_kind"));
                        }
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
