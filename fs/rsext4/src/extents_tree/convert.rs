use super::{split::SplitInfo, *};

impl<'a> ExtentTree<'a> {
    /// Splits an unwritten extent so `start..start + len` is represented by an
    /// exact, still-unwritten leaf entry. This is the metadata preparation
    /// phase performed before data I/O.
    pub(crate) fn prepare_unwritten_write<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        start: u32,
        len: u32,
    ) -> Ext4Result<()> {
        self.bind_geometry(&fs.superblock);
        if len == 0 || len > Ext4Extent::EXT_UNINIT_MAX_LEN.into() {
            return Err(Ext4Error::invalid_input().with_operation("extent:unwritten_length"));
        }
        start
            .checked_add(len)
            .ok_or_else(Ext4Error::file_too_large)?;

        let mut root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, block_dev.total_blocks(), true)?;
        let split =
            self.split_extent_recursive(fs, block_dev, &mut root, start, len, true, true, None)?;
        match split {
            None => self.store_root_to_inode(&root),
            Some(right) => self.promote_split_root(fs, block_dev, root, right),
        }
    }

    /// Splits an initialized extent and marks the exact selected range
    /// unwritten without changing its physical allocation.
    pub(crate) fn prepare_initialized_zero<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        start: u32,
        len: u32,
    ) -> Ext4Result<()> {
        self.bind_geometry(&fs.superblock);
        if len == 0 || len > Ext4Extent::EXT_UNINIT_MAX_LEN.into() {
            return Err(Ext4Error::invalid_input().with_operation("extent:zero_length"));
        }
        start
            .checked_add(len)
            .ok_or_else(Ext4Error::file_too_large)?;

        let mut root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, block_dev.total_blocks(), true)?;
        let split =
            self.split_extent_recursive(fs, block_dev, &mut root, start, len, false, true, None)?;
        match split {
            None => self.store_root_to_inode(&root),
            Some(right) => self.promote_split_root(fs, block_dev, root, right),
        }
    }

    /// Marks an exact prepared unwritten extent initialized after its data I/O
    /// has completed successfully.
    pub(crate) fn finish_unwritten_write<B: BlockIo>(
        &mut self,
        block_dev: &mut Jbd2Dev<B>,
        start: u32,
        len: u32,
    ) -> Ext4Result<()> {
        let mut root = self.load_root_from_inode()?;
        self.validate_node(&root, None, None, block_dev.total_blocks(), true)?;
        self.initialize_exact_recursive(block_dev, &mut root, start, len, None)?;
        self.store_root_to_inode(&root)
    }

    #[allow(clippy::too_many_arguments)]
    fn split_extent_recursive<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        node: &mut ExtentNode,
        start: u32,
        len: u32,
        expected_unwritten: bool,
        middle_unwritten: bool,
        physical_node: Option<AbsoluteBN>,
    ) -> Ext4Result<Option<SplitInfo>> {
        match node {
            ExtentNode::Leaf { header, entries } => {
                let position = entries
                    .iter()
                    .position(|extent| {
                        extent.ee_block <= start
                            && start < extent.ee_block.saturating_add(extent.len())
                    })
                    .ok_or_else(|| {
                        Ext4Error::invalid_input().with_operation("extent:unwritten_missing")
                    })?;
                let original = entries[position];
                if original.is_unwritten() != expected_unwritten {
                    return Err(Ext4Error::invalid_input().with_operation("extent:rewrite_state"));
                }
                let original_end =
                    original
                        .ee_block
                        .checked_add(original.len())
                        .ok_or_else(|| {
                            Ext4Error::corrupted().with_operation("extent:logical_overflow")
                        })?;
                let write_end = start
                    .checked_add(len)
                    .ok_or_else(Ext4Error::file_too_large)?;
                if write_end > original_end {
                    return Err(
                        Ext4Error::invalid_input().with_operation("extent:unwritten_crossing")
                    );
                }

                let mut replacement = Vec::with_capacity(3);
                let left_len = start - original.ee_block;
                if left_len != 0 {
                    replacement.push(build_extent_with_state(
                        original.ee_block,
                        original.start_block(),
                        left_len,
                        original.is_unwritten(),
                        "extent:rewrite_left",
                    )?);
                }
                let middle_physical = original
                    .start_block()
                    .checked_add(u64::from(left_len))
                    .ok_or_else(Ext4Error::overflow)?;
                replacement.push(build_extent_with_state(
                    start,
                    middle_physical,
                    len,
                    middle_unwritten,
                    "extent:rewrite_middle",
                )?);
                let right_len = original_end - write_end;
                if right_len != 0 {
                    let right_physical = middle_physical
                        .checked_add(u64::from(len))
                        .ok_or_else(Ext4Error::overflow)?;
                    replacement.push(build_extent_with_state(
                        write_end,
                        right_physical,
                        right_len,
                        original.is_unwritten(),
                        "extent:rewrite_right",
                    )?);
                }

                entries.splice(position..=position, replacement);
                header.eh_entries = u16::try_from(entries.len())
                    .map_err(|_| Ext4Error::corrupted().with_operation("extent:entry_overflow"))?;
                if entries.len() <= usize::from(header.eh_max) {
                    if let Some(block) = physical_node {
                        let disk_node = ExtentNode::Leaf {
                            header: *header,
                            entries: entries.clone(),
                        };
                        self.write_node_to_block(block_dev, block, &disk_node)?;
                    }
                    return Ok(None);
                }
                self.split_leaf_after_rewrite(fs, block_dev, header, entries, physical_node)
                    .map(Some)
            }
            ExtentNode::Index { header, entries } => {
                let partition = entries.partition_point(|index| index.ei_block <= start);
                let position = partition.saturating_sub(1);
                let child_index = entries
                    .get(position)
                    .copied()
                    .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:empty_index"))?;
                let child_block = AbsoluteBN::new(
                    (u64::from(child_index.ei_leaf_hi) << 32) | u64::from(child_index.ei_leaf_lo),
                );
                let mut child =
                    self.read_child_node(block_dev, &child_index, header.eh_depth - 1)?;
                let child_split = self.split_extent_recursive(
                    fs,
                    block_dev,
                    &mut child,
                    start,
                    len,
                    expected_unwritten,
                    middle_unwritten,
                    Some(child_block),
                )?;
                entries[position].ei_block = Self::get_node_start_block(&child);
                if let Some(split) = child_split {
                    let new_index = Ext4ExtentIdx {
                        ei_block: split.start_block,
                        ei_leaf_lo: split.phy_block.raw() as u32,
                        ei_leaf_hi: (split.phy_block.raw() >> 32) as u16,
                        ei_unused: 0,
                    };
                    let insert_at =
                        entries.partition_point(|index| index.ei_block < split.start_block);
                    entries.insert(insert_at, new_index);
                    header.eh_entries = u16::try_from(entries.len()).map_err(|_| {
                        Ext4Error::corrupted().with_operation("extent:index_overflow")
                    })?;
                }
                if entries.len() <= usize::from(header.eh_max) {
                    if let Some(block) = physical_node {
                        let disk_node = ExtentNode::Index {
                            header: *header,
                            entries: entries.clone(),
                        };
                        self.write_node_to_block(block_dev, block, &disk_node)?;
                    }
                    return Ok(None);
                }
                self.split_index_after_rewrite(fs, block_dev, header, entries, physical_node)
                    .map(Some)
            }
        }
    }

    fn split_leaf_after_rewrite<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        header: &mut Ext4ExtentHeader,
        entries: &mut Vec<Ext4Extent>,
        physical_node: Option<AbsoluteBN>,
    ) -> Ext4Result<SplitInfo> {
        let right_entries = entries.split_off(entries.len() / 2);
        header.eh_entries = entries.len() as u16;
        self.can_add_inode_sectors_for_block(fs)?;
        let right_block = fs.alloc_block(block_dev)?;
        self.add_inode_sectors_for_block(fs)?;
        let right_node = ExtentNode::Leaf {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: right_entries.len() as u16,
                eh_max: self.calc_block_eh_max(),
                eh_depth: 0,
                eh_generation: 0,
            },
            entries: right_entries,
        };
        self.write_node_to_block(block_dev, right_block, &right_node)?;
        if let Some(left_block) = physical_node {
            let left_node = ExtentNode::Leaf {
                header: *header,
                entries: entries.clone(),
            };
            self.write_node_to_block(block_dev, left_block, &left_node)?;
        }
        Ok(SplitInfo {
            start_block: Self::get_node_start_block(&right_node),
            phy_block: right_block,
        })
    }

    fn split_index_after_rewrite<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        header: &mut Ext4ExtentHeader,
        entries: &mut Vec<Ext4ExtentIdx>,
        physical_node: Option<AbsoluteBN>,
    ) -> Ext4Result<SplitInfo> {
        let right_entries = entries.split_off(entries.len() / 2);
        header.eh_entries = entries.len() as u16;
        self.can_add_inode_sectors_for_block(fs)?;
        let right_block = fs.alloc_block(block_dev)?;
        self.add_inode_sectors_for_block(fs)?;
        let right_node = ExtentNode::Index {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: right_entries.len() as u16,
                eh_max: self.calc_block_eh_max(),
                eh_depth: header.eh_depth,
                eh_generation: 0,
            },
            entries: right_entries,
        };
        self.write_node_to_block(block_dev, right_block, &right_node)?;
        if let Some(left_block) = physical_node {
            let left_node = ExtentNode::Index {
                header: *header,
                entries: entries.clone(),
            };
            self.write_node_to_block(block_dev, left_block, &left_node)?;
        }
        Ok(SplitInfo {
            start_block: Self::get_node_start_block(&right_node),
            phy_block: right_block,
        })
    }

    fn promote_split_root<B: BlockIo>(
        &mut self,
        fs: &mut Ext4FileSystem,
        block_dev: &mut Jbd2Dev<B>,
        left_root: ExtentNode,
        right: SplitInfo,
    ) -> Ext4Result<()> {
        self.can_add_inode_sectors_for_block(fs)?;
        let left_block = fs.alloc_block(block_dev)?;
        self.add_inode_sectors_for_block(fs)?;
        self.write_node_to_block(block_dev, left_block, &left_root)?;
        let depth = left_root
            .header()
            .eh_depth
            .checked_add(1)
            .filter(|depth| *depth <= Self::MAX_DEPTH)
            .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:depth_overflow"))?;
        let root = ExtentNode::Index {
            header: Ext4ExtentHeader {
                eh_magic: Ext4ExtentHeader::EXT4_EXT_MAGIC,
                eh_entries: 2,
                eh_max: 4,
                eh_depth: depth,
                eh_generation: 0,
            },
            entries: vec![
                Ext4ExtentIdx {
                    ei_block: Self::get_node_start_block(&left_root),
                    ei_leaf_lo: left_block.raw() as u32,
                    ei_leaf_hi: (left_block.raw() >> 32) as u16,
                    ei_unused: 0,
                },
                Ext4ExtentIdx {
                    ei_block: right.start_block,
                    ei_leaf_lo: right.phy_block.raw() as u32,
                    ei_leaf_hi: (right.phy_block.raw() >> 32) as u16,
                    ei_unused: 0,
                },
            ],
        };
        self.store_root_to_inode(&root)
    }

    fn initialize_exact_recursive<B: BlockIo>(
        &self,
        block_dev: &mut Jbd2Dev<B>,
        node: &mut ExtentNode,
        start: u32,
        len: u32,
        physical_node: Option<AbsoluteBN>,
    ) -> Ext4Result<()> {
        match node {
            ExtentNode::Leaf { header, entries } => {
                let position = entries
                    .iter_mut()
                    .position(|extent| extent.ee_block == start && extent.len() == len)
                    .ok_or_else(|| {
                        Ext4Error::corrupted().with_operation("extent:prepared_range_missing")
                    })?;
                let extent = &mut entries[position];
                if !extent.is_unwritten() {
                    return Err(
                        Ext4Error::corrupted().with_operation("extent:prepared_range_initialized")
                    );
                }
                extent.ee_len = Ext4Extent::encode_len(len, false).ok_or_else(|| {
                    Ext4Error::corrupted().with_operation("extent:initialized_length")
                })?;
                Self::merge_leaf_extent_neighbors(entries, position)?;
                header.eh_entries = u16::try_from(entries.len())
                    .map_err(|_| Ext4Error::corrupted().with_operation("extent:entry_overflow"))?;
                if let Some(block) = physical_node {
                    let disk_node = ExtentNode::Leaf {
                        header: *header,
                        entries: entries.clone(),
                    };
                    self.write_node_to_block(block_dev, block, &disk_node)?;
                }
                Ok(())
            }
            ExtentNode::Index { header, entries } => {
                let partition = entries.partition_point(|index| index.ei_block <= start);
                let position = partition.saturating_sub(1);
                let index = entries
                    .get(position)
                    .ok_or_else(|| Ext4Error::corrupted().with_operation("extent:empty_index"))?;
                let child_block = AbsoluteBN::new(
                    (u64::from(index.ei_leaf_hi) << 32) | u64::from(index.ei_leaf_lo),
                );
                let mut child = self.read_child_node(block_dev, index, header.eh_depth - 1)?;
                self.initialize_exact_recursive(
                    block_dev,
                    &mut child,
                    start,
                    len,
                    Some(child_block),
                )
            }
        }
    }
}

fn build_extent_with_state(
    logical: u32,
    physical: u64,
    len: u32,
    unwritten: bool,
    operation: &'static str,
) -> Ext4Result<Ext4Extent> {
    let extent = if unwritten {
        Ext4Extent::new_unwritten(logical, physical, len)
    } else {
        u16::try_from(len)
            .ok()
            .map(|len| Ext4Extent::new(logical, physical, len))
    };
    extent.ok_or_else(|| Ext4Error::corrupted().with_operation(operation))
}
