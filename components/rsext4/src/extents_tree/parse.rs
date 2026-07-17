use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtentRun {
    pub logical_start: u32,
    pub physical_start: AbsoluteBN,
    pub len: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExtentMappingState {
    Initialized,
    Unwritten,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExtentMappingRun {
    pub logical_start: u32,
    pub physical_start: AbsoluteBN,
    pub len: u32,
    pub state: ExtentMappingState,
}

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

    pub fn initialized_runs_in_range<B: BlockDevice>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        start_lbn: u32,
        end_lbn: u32,
    ) -> Ext4Result<Vec<ExtentRun>> {
        Ok(self
            .mapped_runs_in_range(dev, start_lbn, end_lbn)?
            .into_iter()
            .filter_map(|run| {
                (run.state == ExtentMappingState::Initialized).then_some(ExtentRun {
                    logical_start: run.logical_start,
                    physical_start: run.physical_start,
                    len: run.len,
                })
            })
            .collect())
    }

    pub(crate) fn mapped_runs_in_range<B: BlockDevice>(
        &mut self,
        dev: &mut Jbd2Dev<B>,
        start_lbn: u32,
        end_lbn: u32,
    ) -> Ext4Result<Vec<ExtentMappingRun>> {
        if start_lbn > end_lbn {
            return Ok(Vec::new());
        }
        let Some(root) = self.load_root_from_inode() else {
            return Ok(Vec::new());
        };
        let mut runs = Vec::new();
        Self::collect_mappings_in_node(dev, &root, start_lbn, end_lbn, &mut runs)?;
        runs.sort_unstable_by_key(|run| run.logical_start);
        Ok(runs)
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
                    let len = et.len();
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

    fn collect_mappings_in_node<B: BlockDevice>(
        dev: &mut Jbd2Dev<B>,
        node: &ExtentNode,
        start_lbn: u32,
        end_lbn: u32,
        out: &mut Vec<ExtentMappingRun>,
    ) -> Ext4Result<()> {
        match node {
            ExtentNode::Leaf { entries, .. } => {
                for ext in entries {
                    let len = ext.len();
                    if len == 0 {
                        continue;
                    }
                    let ext_start = ext.ee_block;
                    let ext_end = ext_start.saturating_add(len).saturating_sub(1);
                    if ext_end < start_lbn || ext_start > end_lbn {
                        continue;
                    }
                    let logical_start = ext_start.max(start_lbn);
                    let logical_end = ext_end.min(end_lbn);
                    let physical_offset = logical_start.saturating_sub(ext_start);
                    let physical_start =
                        AbsoluteBN::new(ext.start_block()).checked_add(physical_offset)?;
                    out.push(ExtentMappingRun {
                        logical_start,
                        physical_start,
                        len: logical_end.saturating_sub(logical_start).saturating_add(1),
                        state: if ext.is_unwritten() {
                            ExtentMappingState::Unwritten
                        } else {
                            ExtentMappingState::Initialized
                        },
                    });
                }
                Ok(())
            }
            ExtentNode::Index { entries, .. } => {
                for (idx, entry) in entries.iter().enumerate() {
                    let child_start = entry.ei_block;
                    let child_end = entries
                        .get(idx + 1)
                        .map(|next| next.ei_block.saturating_sub(1))
                        .unwrap_or(u32::MAX);
                    if child_end < start_lbn || child_start > end_lbn {
                        continue;
                    }
                    let child_block = AbsoluteBN::new(
                        ((entry.ei_leaf_hi as u64) << 32) | entry.ei_leaf_lo as u64,
                    );
                    dev.read_block(child_block)?;
                    let child =
                        Self::parse_node_from_bytes(dev.buffer()).ok_or(Ext4Error::corrupted())?;
                    Self::collect_mappings_in_node(dev, &child, start_lbn, end_lbn, out)?;
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
        blockdev::{BlockDevice, Jbd2Dev},
        bmalloc::AbsoluteBN,
        disknode::Ext4Timestamp,
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
            Self {
                data: vec![0; total_blocks as usize * BLOCK_SIZE],
                total_blocks,
                now: Cell::new(1_700_000_000),
            }
        }
    }

    impl BlockDevice for MemBlockDev {
        fn write(&mut self, buffer: &[u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
            self.data[start..end].copy_from_slice(&buffer[..required]);
            Ok(())
        }

        fn read(&mut self, buffer: &mut [u8], block_id: AbsoluteBN, count: u32) -> Ext4Result<()> {
            let required = BLOCK_SIZE * count as usize;
            if buffer.len() < required {
                return Err(Ext4Error::buffer_too_small(buffer.len(), required));
            }
            let start = block_id.as_usize()? * BLOCK_SIZE;
            let end = start + required;
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

    fn alloc_contiguous<B: BlockDevice>(
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

    #[test]
    fn extent_runs_clip_single_extent_to_requested_range() {
        let (mut dev, mut fs) = setup_fs(16 * 1024);
        let mut inode = new_extent_inode();
        let base = alloc_contiguous(&mut fs, &mut dev, 10);
        {
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, Ext4Extent::new(10, base.raw(), 10), &mut dev)
                .unwrap();
        }

        let mut tree = ExtentTree::new(&mut inode);
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
            let mut tree = ExtentTree::new(&mut inode);
            tree.insert_extent(&mut fs, Ext4Extent::new(0, base1.raw(), 2), &mut dev)
                .unwrap();
            tree.insert_extent(&mut fs, Ext4Extent::new(4, base2.raw(), 2), &mut dev)
                .unwrap();
        }

        let mut tree = ExtentTree::new(&mut inode);
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
}
