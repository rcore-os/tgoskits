//! Immutable physical targets and bounded scratch for partial-block writes.

use core::ops::Range;

use super::*;

pub(super) struct FileDataWrites {
    requests: Vec<DataWrite>,
    input_length: usize,
}

enum DataWrite {
    Full {
        physical: AbsoluteBN,
        count: u32,
        source: Range<usize>,
    },
    Partial(PartialBlockWrite),
}

struct PartialBlockWrite {
    physical: AbsoluteBN,
    source: Range<usize>,
    destination: usize,
    bytes: Vec<u8>,
    original: OriginalBlock,
}

enum OriginalBlock {
    ReadHome,
    Prepared,
}

impl FileDataWrites {
    pub(super) fn prepare<B: BlockIo>(
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        number: InodeNumber,
        bytes: Range<u64>,
    ) -> Ext4Result<Self> {
        let block_size = fs.block_size();
        let block_bytes = block_size as u64;
        let input_length = bytes
            .end
            .checked_sub(bytes.start)
            .and_then(|length| usize::try_from(length).ok())
            .filter(|length| *length != 0)
            .ok_or_else(Ext4Error::invalid_input)?;
        let mut writes = Self {
            requests: Vec::new(),
            input_length,
        };
        let mut inode = fs.get_inode_by_num(device, number)?;
        let mut logical = bytes.start / block_bytes;
        let end_logical = (bytes.end - 1) / block_bytes;
        while logical <= end_logical {
            let extent = ExtentTree::with_filesystem(&mut inode, fs, number)
                .find_extent_at_or_after(device, logical as u32)?
                .ok_or_else(|| Ext4Error::corrupted().with_operation("write:prepared_hole"))?;
            if u64::from(extent.ee_block) > logical {
                return Err(Ext4Error::corrupted().with_operation("write:prepared_hole"));
            }
            let extent_end = u64::from(extent.ee_block)
                .checked_add(u64::from(extent.len()))
                .ok_or_else(Ext4Error::overflow)?;
            if extent_end <= logical {
                return Err(Ext4Error::corrupted().with_operation("write:empty_extent"));
            }
            let run_end = extent_end.min(end_logical + 1);
            while logical < run_end {
                let physical = AbsoluteBN::new(extent.start_block())
                    .checked_add((logical - u64::from(extent.ee_block)) as u32)?;
                let block_start = logical
                    .checked_mul(block_bytes)
                    .ok_or_else(Ext4Error::overflow)?;
                let block_end = block_start
                    .checked_add(block_bytes)
                    .ok_or_else(Ext4Error::overflow)?;
                let start = bytes.start.max(block_start);
                let end = bytes.end.min(block_end);
                let source_start =
                    usize::try_from(start - bytes.start).map_err(|_| Ext4Error::overflow())?;
                let source_end =
                    usize::try_from(end - bytes.start).map_err(|_| Ext4Error::overflow())?;
                let cached = fs.datablock_cache.take_clean_file_image(physical)?;
                if start == block_start && end == block_end {
                    writes.push_full(physical, source_start..source_end, block_size)?;
                } else {
                    let mut contents = Vec::new();
                    contents
                        .try_reserve_exact(block_size)
                        .map_err(|_| Ext4Error::no_memory())?;
                    contents.resize(block_size, 0);
                    let original = if extent.is_unwritten() {
                        OriginalBlock::Prepared
                    } else if let Some(cached) = cached {
                        if cached.len() != block_size {
                            return Err(
                                Ext4Error::corrupted().with_operation("write:cache_block_size")
                            );
                        }
                        contents.copy_from_slice(&cached);
                        OriginalBlock::Prepared
                    } else {
                        OriginalBlock::ReadHome
                    };
                    writes
                        .requests
                        .try_reserve(1)
                        .map_err(|_| Ext4Error::no_memory())?;
                    writes.requests.push(DataWrite::Partial(PartialBlockWrite {
                        physical,
                        source: source_start..source_end,
                        destination: usize::try_from(start - block_start)
                            .map_err(|_| Ext4Error::overflow())?,
                        bytes: contents,
                        original,
                    }));
                }
                logical += 1;
            }
        }
        Ok(writes)
    }

    fn push_full(
        &mut self,
        physical: AbsoluteBN,
        source: Range<usize>,
        block_size: usize,
    ) -> Ext4Result<()> {
        if let Some(DataWrite::Full {
            physical: first,
            count,
            source: previous,
        }) = self.requests.last_mut()
            && first.checked_add(*count)? == physical
            && previous.end == source.start
            && source.end - previous.start <= super::super::super::io::MAX_RUN_IO_BYTES
        {
            *count = count.checked_add(1).ok_or_else(Ext4Error::overflow)?;
            previous.end = source.end;
            return Ok(());
        }
        if source.end - source.start != block_size {
            return Err(Ext4Error::invalid_input().with_operation("write:full_block_range"));
        }
        self.requests
            .try_reserve(1)
            .map_err(|_| Ext4Error::no_memory())?;
        self.requests.push(DataWrite::Full {
            physical,
            count: 1,
            source,
        });
        Ok(())
    }

    pub(super) fn execute<B: BlockIo>(
        self,
        device: &mut FileDataEndpoint<B>,
        input: &[u8],
    ) -> Ext4Result<()> {
        if input.len() != self.input_length {
            return Err(Ext4Error::invalid_input().with_operation("write:input_length"));
        }
        for request in self.requests {
            match request {
                DataWrite::Full {
                    physical,
                    count,
                    source,
                } => {
                    device.write_blocks(&input[source], physical, count)?;
                }
                DataWrite::Partial(mut partial) => {
                    if matches!(partial.original, OriginalBlock::ReadHome) {
                        device.read_blocks(&mut partial.bytes, partial.physical, 1)?;
                    }
                    let source = &input[partial.source];
                    let end = partial.destination + source.len();
                    partial.bytes[partial.destination..end].copy_from_slice(source);
                    device.write_blocks(&partial.bytes, partial.physical, 1)?;
                }
            }
        }
        Ok(())
    }
}
