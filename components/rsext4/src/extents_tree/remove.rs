use super::*;

impl<'a> ExtentTree<'a> {
    /// Removes an allocated logical extent range from the tree.
    ///
    /// The algorithm first validates that the requested range maps to real
    /// extents, then walks the tree again to free blocks and rewrite touched
    /// leaf/index nodes, and finally collapses degenerate root states back into
    /// the inode-inline form when possible.
    pub fn remove_extend<B: BlockDevice>(
        &mut self,
        fs: &mut Ext4FileSystem,
        deleted_ext: Ext4Extent,
        block_dev: &mut Jbd2Dev<B>,
    ) -> Ext4Result<()> {
        let del_start = deleted_ext.ee_block;
        let del_len = (deleted_ext.ee_len as u32) & 0x7FFF;
        if del_len == 0 {
            return Ok(());
        }

        // Phase 1: validate the full deletion span before mutating any extent node.
        // Holes are skipped during validation; only allocated blocks count toward del_len.
        {
            #[derive(Clone, Copy)]
            enum PreKind {
                Have,
                HoleSkip,
                NoMore,
            }

            #[derive(Clone, Copy)]
            struct PreRes {
                kind: PreKind,
                can_take: u32,
                next_lbn: u32,
            }

            fn extent_len15(e: &Ext4Extent) -> u32 {
                (e.ee_len as u32) & 0x7FFF
            }

            // A leaf either contributes a deletable segment or points the walker at the next hole boundary.
            fn pre_leaf_step(entries: &[Ext4Extent], cur_lbn: u32) -> PreRes {
                let mut best: Option<&Ext4Extent> = None;
                for e in entries {
                    let len = extent_len15(e);
                    if len == 0 {
                        continue;
                    }
                    let start = e.ee_block;
                    let end = start.saturating_add(len);
                    if start <= cur_lbn && cur_lbn < end {
                        best = Some(e);
                        break;
                    }
                    if cur_lbn < start {
                        best = Some(e);
                        break;
                    }
                }

                let Some(e) = best else {
                    return PreRes {
                        kind: PreKind::NoMore,
                        can_take: 0,
                        next_lbn: cur_lbn,
                    };
                };

                let len15 = extent_len15(e);
                if len15 == 0 {
                    return PreRes {
                        kind: PreKind::NoMore,
                        can_take: 0,
                        next_lbn: cur_lbn,
                    };
                }
                let e_start = e.ee_block;
                let e_end = e_start.saturating_add(len15);

                if cur_lbn < e_start {
                    return PreRes {
                        kind: PreKind::HoleSkip,
                        can_take: 0,
                        next_lbn: e_start,
                    };
                }

                let within_off = cur_lbn.saturating_sub(e_start);
                let can_take = len15.saturating_sub(within_off);
                if can_take == 0 {
                    return PreRes {
                        kind: PreKind::HoleSkip,
                        can_take: 0,
                        next_lbn: e_end,
                    };
                }

                PreRes {
                    kind: PreKind::Have,
                    can_take,
                    next_lbn: cur_lbn,
                }
            }

            // Recursively search the next child that could satisfy the requested logical block.
            fn pre_step<B: BlockDevice>(
                dev: &mut Jbd2Dev<B>,
                node: &ExtentNode,
                cur_lbn: u32,
            ) -> Ext4Result<PreRes> {
                match node {
                    ExtentNode::Leaf { entries, .. } => Ok(pre_leaf_step(entries, cur_lbn)),
                    ExtentNode::Index { entries, .. } => {
                        if entries.is_empty() {
                            return Ok(PreRes {
                                kind: PreKind::NoMore,
                                can_take: 0,
                                next_lbn: cur_lbn,
                            });
                        }

                        let mut search_lbn = cur_lbn;
                        let mut idx_pos = {
                            let pp = entries.partition_point(|idx| idx.ei_block <= search_lbn);
                            if pp == 0 { 0 } else { pp - 1 }
                        };

                        while idx_pos < entries.len() {
                            let child_phy = AbsoluteBN::new(
                                ((entries[idx_pos].ei_leaf_hi as u64) << 32)
                                    | (entries[idx_pos].ei_leaf_lo as u64),
                            );
                            dev.read_block(child_phy)?;
                            let child = ExtentTree::parse_node_from_bytes(dev.buffer())
                                .ok_or(Ext4Error::corrupted())?;

                            let r = pre_step(dev, &child, search_lbn)?;
                            match r.kind {
                                PreKind::Have | PreKind::HoleSkip => return Ok(r),
                                PreKind::NoMore => {
                                    idx_pos += 1;
                                    if idx_pos < entries.len() {
                                        search_lbn = entries[idx_pos].ei_block;
                                        continue;
                                    }
                                    break;
                                }
                            }
                        }

                        Ok(PreRes {
                            kind: PreKind::NoMore,
                            can_take: 0,
                            next_lbn: cur_lbn,
                        })
                    }
                }
            }

            let pre_root = match self.load_root_from_inode() {
                Some(node) => node,
                None => return Err(Ext4Error::corrupted()),
            };

            let mut need = del_len;
            let mut cur = del_start;
            while need > 0 {
                let r = pre_step(block_dev, &pre_root, cur)?;
                match r.kind {
                    PreKind::Have => {
                        let take = core::cmp::min(need, r.can_take);
                        need = need.saturating_sub(take);
                        cur = cur.saturating_add(take);
                    }
                    PreKind::HoleSkip => {
                        if r.next_lbn <= cur {
                            return Err(Ext4Error::corrupted());
                        }
                        cur = r.next_lbn;
                    }
                    PreKind::NoMore => return Err(Ext4Error::invalid_input()),
                }
            }
        }

        // Phase 2: perform the actual deletion and rewrite touched nodes on unwind.
        let mut root = match self.load_root_from_inode() {
            Some(node) => node,
            None => return Err(Ext4Error::corrupted()),
        };

        fn inline_eh_max_for_node(node: &ExtentNode) -> u16 {
            let inline_bytes = 15usize * 4;
            let hdr_size = Ext4ExtentHeader::disk_size();
            let entry_size = match node {
                ExtentNode::Leaf { .. } => Ext4Extent::disk_size(),
                ExtentNode::Index { .. } => Ext4ExtentIdx::disk_size(),
            };
            (inline_bytes.saturating_sub(hdr_size) / entry_size) as u16
        }

        fn extent_len15(e: &Ext4Extent) -> u32 {
            (e.ee_len as u32) & 0x7FFF
        }

        fn extent_start_phys(e: &Ext4Extent) -> u64 {
            ((e.ee_start_hi as u64) << 32) | (e.ee_start_lo as u64)
        }

        fn build_extent_len(orig_ee_len: u16, new_len15: u32) -> Ext4Result<u16> {
            if new_len15 > 0x7FFF {
                return Err(Ext4Error::corrupted());
            }
            Ok((orig_ee_len & 0x8000) | (new_len15 as u16))
        }

        #[derive(Clone, Copy)]
        enum StepKind {
            Deleted,
            HoleSkip,
            NoMoreExtent,
        }

        #[derive(Clone, Copy)]
        struct StepRes {
            kind: StepKind,
            deleted: u32,
            next_lbn: u32,
            empty: bool,
            first_key: u32,
        }

        fn first_key_of_node(node: &ExtentNode) -> u32 {
            match node {
                ExtentNode::Leaf { entries, .. } => {
                    entries.first().map(|e| e.ee_block).unwrap_or(0)
                }
                ExtentNode::Index { entries, .. } => {
                    entries.first().map(|e| e.ei_block).unwrap_or(0)
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn leaf_step<'t, B: BlockDevice>(
            tree: &mut ExtentTree<'t>,
            fs: &mut Ext4FileSystem,
            dev: &mut Jbd2Dev<B>,
            header: &mut Ext4ExtentHeader,
            entries: &mut Vec<Ext4Extent>,
            cur_lbn: u32,
            remaining: u32,
            phy_block: Option<AbsoluteBN>,
        ) -> Ext4Result<StepRes> {
            if entries.is_empty() {
                return Ok(StepRes {
                    kind: StepKind::NoMoreExtent,
                    deleted: 0,
                    next_lbn: cur_lbn,
                    empty: true,
                    first_key: 0,
                });
            }

            let mut best: Option<usize> = None;
            for (i, e) in entries.iter().enumerate() {
                let len = extent_len15(e);
                if len == 0 {
                    continue;
                }
                let start = e.ee_block;
                let end = start.saturating_add(len);
                if start <= cur_lbn && cur_lbn < end {
                    best = Some(i);
                    break;
                }
                if cur_lbn < start {
                    best = Some(i);
                    break;
                }
            }

            let Some(i) = best else {
                return Ok(StepRes {
                    kind: StepKind::NoMoreExtent,
                    deleted: 0,
                    next_lbn: cur_lbn,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            };

            let e = entries[i];
            let len15 = extent_len15(&e);
            if len15 == 0 {
                return Ok(StepRes {
                    kind: StepKind::NoMoreExtent,
                    deleted: 0,
                    next_lbn: cur_lbn,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            }
            let e_start = e.ee_block;
            let e_end = e_start.saturating_add(len15);

            if cur_lbn < e_start {
                return Ok(StepRes {
                    kind: StepKind::HoleSkip,
                    deleted: 0,
                    next_lbn: e_start,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            }

            let seg_start = cur_lbn;
            let within_off = seg_start.saturating_sub(e_start);
            let can_take = len15.saturating_sub(within_off);
            if can_take == 0 {
                return Ok(StepRes {
                    kind: StepKind::HoleSkip,
                    deleted: 0,
                    next_lbn: e_end,
                    empty: entries.is_empty(),
                    first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
                });
            }
            let cut_len = core::cmp::min(remaining, can_take);
            let seg_end = seg_start.saturating_add(cut_len);

            {
                // Free physical blocks first so allocation bitmaps stay consistent with the extent edit.
                let base = extent_start_phys(&e);
                let off = within_off as u64;
                for j in 0..(cut_len as u64) {
                    fs.free_block(dev, AbsoluteBN::new(base + off + j))?;
                    tree.sub_inode_sectors_for_block();
                }
            }

            // Rewrite the matching extent as delete, trim-left, trim-right, or split-in-two.
            if seg_start == e_start && seg_end == e_end {
                entries.remove(i);
            } else if seg_start == e_start {
                let delta = seg_end.saturating_sub(e_start);
                let new_len15 = len15.saturating_sub(delta);
                let new_start_phys = extent_start_phys(&e) + delta as u64;
                let mut new_e = e;
                new_e.ee_block = seg_end;
                new_e.ee_len = build_extent_len(e.ee_len, new_len15)?;
                new_e.ee_start_lo = (new_start_phys & 0xFFFF_FFFF) as u32;
                new_e.ee_start_hi = (new_start_phys >> 32) as u16;
                entries[i] = new_e;
            } else if seg_end == e_end {
                let new_len15 = seg_start.saturating_sub(e_start);
                let mut new_e = e;
                new_e.ee_len = build_extent_len(e.ee_len, new_len15)?;
                entries[i] = new_e;
            } else {
                let left_len15 = seg_start.saturating_sub(e_start);
                let right_len15 = e_end.saturating_sub(seg_end);

                let mut left_e = e;
                left_e.ee_len = build_extent_len(e.ee_len, left_len15)?;

                let right_start_phys =
                    extent_start_phys(&e) + seg_end.saturating_sub(e_start) as u64;
                let mut right_e = e;
                right_e.ee_block = seg_end;
                right_e.ee_len = build_extent_len(e.ee_len, right_len15)?;
                right_e.ee_start_lo = (right_start_phys & 0xFFFF_FFFF) as u32;
                right_e.ee_start_hi = (right_start_phys >> 32) as u16;

                entries[i] = left_e;
                entries.insert(i + 1, right_e);
            }

            entries.sort_unstable_by_key(|e| e.ee_block);
            header.eh_entries = entries.len() as u16;

            if let Some(block_id) = phy_block {
                let disk_node = ExtentNode::Leaf {
                    header: *header,
                    entries: entries.clone(),
                };
                ExtentTree::write_node_to_block(dev, block_id, &disk_node, header.eh_max)?;
            }

            Ok(StepRes {
                kind: StepKind::Deleted,
                deleted: cut_len,
                next_lbn: seg_end,
                empty: entries.is_empty(),
                first_key: entries.first().map(|e| e.ee_block).unwrap_or(0),
            })
        }

        // Descend to the child covering the current logical block and repair parent keys while unwinding.
        fn step_recursive<'t, B: BlockDevice>(
            tree: &mut ExtentTree<'t>,
            fs: &mut Ext4FileSystem,
            dev: &mut Jbd2Dev<B>,
            node: &mut ExtentNode,
            cur_lbn: u32,
            remaining: u32,
            phy_block: Option<AbsoluteBN>,
        ) -> Ext4Result<StepRes> {
            match node {
                ExtentNode::Leaf { header, entries } => leaf_step(
                    tree, fs, dev, header, entries, cur_lbn, remaining, phy_block,
                ),
                ExtentNode::Index { header, entries } => {
                    if entries.is_empty() {
                        return Ok(StepRes {
                            kind: StepKind::NoMoreExtent,
                            deleted: 0,
                            next_lbn: cur_lbn,
                            empty: true,
                            first_key: 0,
                        });
                    }

                    let mut search_lbn = cur_lbn;
                    let mut idx_pos = {
                        let pp = entries.partition_point(|idx| idx.ei_block <= search_lbn);
                        if pp == 0 { 0 } else { pp - 1 }
                    };

                    while idx_pos < entries.len() {
                        let child_phy = AbsoluteBN::new(
                            ((entries[idx_pos].ei_leaf_hi as u64) << 32)
                                | (entries[idx_pos].ei_leaf_lo as u64),
                        );
                        dev.read_block(child_phy)?;
                        let child_bytes = dev.buffer();
                        let mut child_node = ExtentTree::parse_node_from_bytes(child_bytes)
                            .ok_or(Ext4Error::corrupted())?;

                        let child_res = step_recursive(
                            tree,
                            fs,
                            dev,
                            &mut child_node,
                            search_lbn,
                            remaining,
                            Some(child_phy),
                        )?;

                        match child_res.kind {
                            StepKind::Deleted => {
                                if child_res.empty {
                                    entries.remove(idx_pos);
                                    header.eh_entries = entries.len() as u16;
                                    fs.free_block(dev, child_phy)?;
                                    tree.sub_inode_sectors_for_block();
                                } else {
                                    entries[idx_pos].ei_block = child_res.first_key;
                                }

                                entries.sort_unstable_by_key(|e| e.ei_block);
                                header.eh_entries = entries.len() as u16;

                                if let Some(block_id) = phy_block {
                                    let disk_node = ExtentNode::Index {
                                        header: *header,
                                        entries: entries.clone(),
                                    };
                                    ExtentTree::write_node_to_block(
                                        dev,
                                        block_id,
                                        &disk_node,
                                        header.eh_max,
                                    )?;
                                }

                                return Ok(StepRes {
                                    kind: StepKind::Deleted,
                                    deleted: child_res.deleted,
                                    next_lbn: child_res.next_lbn,
                                    empty: entries.is_empty(),
                                    first_key: entries.first().map(|e| e.ei_block).unwrap_or(0),
                                });
                            }
                            StepKind::HoleSkip => {
                                return Ok(StepRes {
                                    kind: StepKind::HoleSkip,
                                    deleted: 0,
                                    next_lbn: child_res.next_lbn,
                                    empty: false,
                                    first_key: first_key_of_node(node),
                                });
                            }
                            StepKind::NoMoreExtent => {
                                idx_pos += 1;
                                if idx_pos < entries.len() {
                                    search_lbn = entries[idx_pos].ei_block;
                                    continue;
                                }
                                break;
                            }
                        }
                    }

                    Ok(StepRes {
                        kind: StepKind::NoMoreExtent,
                        deleted: 0,
                        next_lbn: search_lbn,
                        empty: false,
                        first_key: first_key_of_node(node),
                    })
                }
            }
        }

        let mut remaining = del_len;
        let mut cur_lbn = del_start;
        let mut changed = false;
        // Consume the deletion span piece by piece because each step may trim only one extent fragment.
        while remaining > 0 {
            let res = step_recursive(self, fs, block_dev, &mut root, cur_lbn, remaining, None)?;
            match res.kind {
                StepKind::Deleted => {
                    if res.deleted == 0 {
                        return Err(Ext4Error::corrupted());
                    }
                    remaining = remaining.saturating_sub(res.deleted);
                    cur_lbn = res.next_lbn;
                    changed = true;
                }
                StepKind::HoleSkip => {
                    if res.next_lbn <= cur_lbn {
                        return Err(Ext4Error::corrupted());
                    }
                    cur_lbn = res.next_lbn;
                }
                StepKind::NoMoreExtent => {
                    return Err(Ext4Error::invalid_input());
                }
            }
        }

        if !changed {
            return Err(Ext4Error::invalid_input());
        }

        // Phase 3: store the updated root, collapsing one-child index roots back into the inode when legal.
        let en_max = inline_eh_max_for_node(&root);
        match &mut root {
            ExtentNode::Leaf { header, entries } => {
                header.eh_entries = entries.len() as u16;
                header.eh_max = en_max;
                self.store_root_to_inode(&root);
                Ok(())
            }
            ExtentNode::Index { header, entries } => {
                if entries.is_empty() {
                    let mut hdr = Ext4ExtentHeader::new();
                    hdr.eh_magic = Ext4ExtentHeader::EXT4_EXT_MAGIC;
                    hdr.eh_depth = 0;
                    hdr.eh_entries = 0;
                    hdr.eh_max = ((15usize * 4usize).saturating_sub(Ext4ExtentHeader::disk_size())
                        / Ext4Extent::disk_size()) as u16;
                    let empty_root = ExtentNode::Leaf {
                        header: hdr,
                        entries: Vec::new(),
                    };
                    self.store_root_to_inode(&empty_root);
                    return Ok(());
                }

                if entries.len() == 1 {
                    let child_phy = AbsoluteBN::new(
                        ((entries[0].ei_leaf_hi as u64) << 32) | (entries[0].ei_leaf_lo as u64),
                    );
                    block_dev.read_block(child_phy)?;
                    let child_bytes = block_dev.buffer();
                    let mut child_node = ExtentTree::parse_node_from_bytes(child_bytes)
                        .ok_or(Ext4Error::corrupted())?;

                    let inline_max = inline_eh_max_for_node(&child_node) as usize;
                    let child_entries_len = match &child_node {
                        ExtentNode::Leaf { entries, .. } => entries.len(),
                        ExtentNode::Index { entries, .. } => entries.len(),
                    };

                    if child_entries_len <= inline_max {
                        *child_node.header_mut() = {
                            let mut h = *child_node.header();
                            h.eh_max = inline_eh_max_for_node(&child_node);
                            h
                        };

                        self.store_root_to_inode(&child_node);

                        fs.free_block(block_dev, child_phy)?;
                        self.sub_inode_sectors_for_block();
                        return Ok(());
                    }
                }

                header.eh_entries = entries.len() as u16;
                header.eh_max = en_max;
                self.store_root_to_inode(&root);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{vec, vec::Vec};
    use core::cell::Cell;

    use super::*;
    use crate::{
        blockdev::{BlockDevice, Jbd2Dev},
        bmalloc::AbsoluteBN,
        cache::bitmap::CacheKey,
        error::{Ext4Error, Ext4Result},
        ext4::{mkfs, mount},
    };

    struct MemBlockDev {
        data: Vec<u8>,
        total_blocks: u64,
        now: Cell<i64>,
    }

    impl MemBlockDev {
        fn new(total_blocks: u64) -> Self {
            let size = total_blocks as usize * BLOCK_SIZE;
            Self {
                data: vec![0u8; size],
                total_blocks,
                now: Cell::new(1_700_000_000),
            }
        }
    }

    impl BlockDevice for MemBlockDev {
        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let block_size = BLOCK_SIZE;
            let required = block_size * count as usize;
            if buffer.len() < required {
                return Err(Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * block_size;
            let end = start + required;
            if end > self.data.len() {
                return Err(Ext4Error::block_out_of_range(
                    block_id.to_u32()?,
                    self.total_blocks,
                ));
            }
            self.data[start..end].copy_from_slice(&buffer[..required]);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let block_size = BLOCK_SIZE;
            let required = block_size * count as usize;
            if buffer.len() < required {
                return Err(Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * block_size;
            let end = start + required;
            if end > self.data.len() {
                return Err(Ext4Error::block_out_of_range(
                    block_id.to_u32()?,
                    self.total_blocks,
                ));
            }
            buffer[..required].copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn open(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn close(&mut self) -> Ext4Result<()> {
            Ok(())
        }

        fn total_blocks(&self) -> u64 {
            self.total_blocks
        }

        fn block_size(&self) -> u32 {
            BLOCK_SIZE as u32
        }

        fn current_time(&self) -> Ext4Result<Ext4Timestamp> {
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

    fn alloc_data_block<B: BlockDevice>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
    ) -> AbsoluteBN {
        fs.alloc_block(dev).unwrap()
    }

    fn bitmap_block_is_allocated<B: BlockDevice>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
        global_block: AbsoluteBN,
    ) -> bool {
        let (group_idx, block_in_group) = fs.block_allocator.global_to_group(global_block).unwrap();
        let desc = fs
            .group_descs
            .get(group_idx.as_usize().unwrap())
            .expect("invalid group_idx");
        let bitmap_block = AbsoluteBN::new(desc.block_bitmap());
        let key = CacheKey::new_block(group_idx);

        let bm = fs
            .bitmap_cache
            .get_or_load(dev, key, bitmap_block)
            .expect("load block bitmap failed");

        let idx = block_in_group.as_usize().unwrap();
        let byte = bm.data[idx / 8];
        ((byte >> (idx % 8)) & 1) == 1
    }

    fn insert_n_extents_with_phys_gaps<B: BlockDevice>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
        inode: &mut Ext4Inode,
        n: u32,
    ) -> std::vec::Vec<Ext4Extent> {
        let mut tree = ExtentTree::new(inode);
        let mut out = std::vec::Vec::new();
        for lbn in 0..n {
            let phys = alloc_data_block(fs, dev);
            let _gap = alloc_data_block(fs, dev);
            let ext = Ext4Extent::new(lbn, phys.raw(), 1);
            tree.insert_extent(fs, ext, dev).unwrap();
            out.push(ext);
        }
        out
    }

    fn alloc_contiguous<B: BlockDevice>(
        fs: &mut Ext4FileSystem,
        dev: &mut Jbd2Dev<B>,
        count: u32,
    ) -> AbsoluteBN {
        assert!(count > 0);
        let first = alloc_data_block(fs, dev);
        let mut prev = first;
        for _ in 1..count {
            let b = alloc_data_block(fs, dev);
            assert_eq!(b, prev.checked_add(1).unwrap());
            prev = b;
        }
        first
    }

    fn collect_extents_from_inode<B: BlockDevice>(
        inode: &mut Ext4Inode,
        dev: &mut Jbd2Dev<B>,
    ) -> std::vec::Vec<Ext4Extent> {
        fn walk<B: BlockDevice>(
            dev: &mut Jbd2Dev<B>,
            node: &ExtentNode,
            out: &mut std::vec::Vec<Ext4Extent>,
        ) {
            match node {
                ExtentNode::Leaf { entries, .. } => out.extend_from_slice(entries),
                ExtentNode::Index { entries, .. } => {
                    for idx in entries {
                        let child_phy = ((idx.ei_leaf_hi as u64) << 32) | (idx.ei_leaf_lo as u64);
                        dev.read_block(AbsoluteBN::new(child_phy)).unwrap();
                        let child =
                            ExtentTree::parse_node_from_bytes(dev.buffer()).expect("parse child");
                        walk(dev, &child, out);
                    }
                }
            }
        }

        let tree = ExtentTree::new(inode);
        let root = tree.load_root_from_inode().unwrap();
        let mut out = std::vec::Vec::new();
        walk(dev, &root, &mut out);
        out.sort_unstable_by_key(|e| e.ee_block);
        out
    }

    #[test]
    fn remove_extend_root_leaf_no_degeneration() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();

        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, exts[0], &mut dev).unwrap();
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].ee_block, 1);
            }
            _ => panic!("expected leaf root"),
        }
    }

    #[test]
    fn remove_extend_root_leaf_degeneration_to_empty() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();

        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 1);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, exts[0], &mut dev).unwrap();
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 0);
            }
            _ => panic!("expected leaf root"),
        }
    }

    #[test]
    fn remove_extend_multilevel_to_root_promotion() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();

        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 5);

        {
            let tree = ExtentTree::new(&mut inode);
            let root = tree.load_root_from_inode().unwrap();
            match root {
                ExtentNode::Index { header, .. } => assert!(header.eh_depth > 0),
                _ => panic!("expected index root after split"),
            }
        }

        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, exts[2], &mut dev).unwrap();
            tree.remove_extend(&mut fs, exts[3], &mut dev).unwrap();
            tree.remove_extend(&mut fs, exts[4], &mut dev).unwrap();
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].ee_block, 0);
                assert_eq!(entries[1].ee_block, 1);
            }
            _ => panic!("expected leaf root after promotion"),
        }
    }

    #[test]
    fn remove_extend_repeated_deletions_consistency() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();
        let exts = insert_n_extents_with_phys_gaps(&mut fs, &mut dev, &mut inode, 5);

        for ext in exts {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, ext, &mut dev).unwrap();

            let tree2 = ExtentTree::new(&mut inode);
            assert!(tree2.load_root_from_inode().is_some());
        }

        let tree = ExtentTree::new(&mut inode);
        let root = tree.load_root_from_inode().unwrap();
        match root {
            ExtentNode::Leaf { header, entries } => {
                assert_eq!(header.eh_depth, 0);
                assert_eq!(entries.len(), 0);
            }
            _ => panic!("expected empty leaf root"),
        }
    }

    #[test]
    fn remove_extend_frees_block_bitmap_bit() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();

        let phys = alloc_data_block(&mut fs, &mut dev);
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, phys));

        let ext = Ext4Extent::new(0, phys.raw(), 1);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, ext, &mut dev).unwrap();
        }

        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, ext, &mut dev).unwrap();
        }

        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, phys));
    }

    #[test]
    fn remove_extend_partial_delete_splits_extent_and_updates_bitmap() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();

        let base = alloc_contiguous(&mut fs, &mut dev, 4);
        let ext = Ext4Extent::new(0, base.raw(), 4);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, ext, &mut dev).unwrap();
        }

        let del = Ext4Extent::new(1, 0, 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, del, &mut dev).unwrap();
        }

        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base));
        assert!(!bitmap_block_is_allocated(
            &mut fs,
            &mut dev,
            base.checked_add(1).unwrap()
        ));
        assert!(!bitmap_block_is_allocated(
            &mut fs,
            &mut dev,
            base.checked_add(2).unwrap()
        ));
        assert!(bitmap_block_is_allocated(
            &mut fs,
            &mut dev,
            base.checked_add(3).unwrap()
        ));

        let exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(exts.len(), 2);
        assert_eq!(exts[0].ee_block, 0);
        assert_eq!((exts[0].ee_len as u32) & 0x7FFF, 1);
        assert_eq!(
            ((exts[0].ee_start_hi as u64) << 32) | (exts[0].ee_start_lo as u64),
            base.raw()
        );
        assert_eq!(exts[1].ee_block, 3);
        assert_eq!((exts[1].ee_len as u32) & 0x7FFF, 1);
        assert_eq!(
            ((exts[1].ee_start_hi as u64) << 32) | (exts[1].ee_start_lo as u64),
            base.checked_add(3).unwrap().raw()
        );
    }

    #[test]
    fn remove_extend_full_delete_single_extent_bitmap_and_metadata() {
        let (mut dev, mut fs) = setup_fs(32 * 1024);
        let mut inode = new_extent_inode();

        let base = alloc_contiguous(&mut fs, &mut dev, 2);
        let ext = Ext4Extent::new(0, base.raw(), 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, ext, &mut dev).unwrap();
        }

        let del = Ext4Extent::new(0, 0, 2);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, del, &mut dev).unwrap();
        }

        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base));
        assert!(!bitmap_block_is_allocated(
            &mut fs,
            &mut dev,
            base.checked_add(1).unwrap()
        ));
        let exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(exts.len(), 0);
    }

    #[test]
    fn remove_extend_multi_extent_skip_hole_and_verify() {
        let (mut dev, mut fs) = setup_fs(64 * 1024);
        let mut inode = new_extent_inode();

        let base1 = alloc_contiguous(&mut fs, &mut dev, 2);
        let _gap1 = alloc_data_block(&mut fs, &mut dev);
        let _gap2 = alloc_data_block(&mut fs, &mut dev);
        let base2 = alloc_contiguous(&mut fs, &mut dev, 2);

        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, Ext4Extent::new(0, base1.raw(), 2), &mut dev)
                .unwrap();
            tree.insert_extent(&mut fs, Ext4Extent::new(4, base2.raw(), 2), &mut dev)
                .unwrap();
        }

        // delete 3 allocated blocks starting at lbn=1: deletes lbn=1, then skips hole [2..4), then deletes lbn=4 and lbn=5
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, Ext4Extent::new(1, 0, 3), &mut dev)
                .unwrap();
        }

        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base1));
        assert!(!bitmap_block_is_allocated(
            &mut fs,
            &mut dev,
            base1.checked_add(1).unwrap()
        ));
        assert!(!bitmap_block_is_allocated(&mut fs, &mut dev, base2));
        assert!(!bitmap_block_is_allocated(
            &mut fs,
            &mut dev,
            base2.checked_add(1).unwrap()
        ));

        let exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].ee_block, 0);
        assert_eq!((exts[0].ee_len as u32) & 0x7FFF, 1);
        assert_eq!(
            ((exts[0].ee_start_hi as u64) << 32) | (exts[0].ee_start_lo as u64),
            base1.raw()
        );
    }

    #[test]
    fn remove_extend_over_length_errors_and_does_not_delete_unrelated() {
        let (mut dev, mut fs) = setup_fs(64 * 1024);
        let mut inode = new_extent_inode();

        let base1 = alloc_contiguous(&mut fs, &mut dev, 2);
        let base2 = alloc_contiguous(&mut fs, &mut dev, 1);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, Ext4Extent::new(0, base1.raw(), 2), &mut dev)
                .unwrap();
            tree.insert_extent(&mut fs, Ext4Extent::new(10, base2.raw(), 1), &mut dev)
                .unwrap();
        }

        let before_exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base1));
        assert!(bitmap_block_is_allocated(
            &mut fs,
            &mut dev,
            base1.checked_add(1).unwrap()
        ));
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base2));

        let res = {
            let mut tree = ExtentTree::new(&mut inode);
            tree.remove_extend(&mut fs, Ext4Extent::new(0, 0, 10), &mut dev)
        };
        assert!(res.is_err());

        // Unrelated extent must remain allocated and metadata should remain unchanged.
        assert!(bitmap_block_is_allocated(&mut fs, &mut dev, base2));
        let after_exts = collect_extents_from_inode(&mut inode, &mut dev);
        assert_eq!(before_exts.len(), after_exts.len());
        for (a, b) in before_exts.iter().zip(after_exts.iter()) {
            assert_eq!(a.ee_block, b.ee_block);
            assert_eq!(a.ee_len, b.ee_len);
            assert_eq!(a.ee_start_hi, b.ee_start_hi);
            assert_eq!(a.ee_start_lo, b.ee_start_lo);
        }
    }
}
