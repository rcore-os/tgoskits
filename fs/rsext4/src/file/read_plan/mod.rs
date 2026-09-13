//! Owned file-block snapshots for callers that protect inode data separately.

mod source;

use alloc::{sync::Arc, vec::Vec};
use core::ops::Range;

use source::MountedFileRead;
pub(crate) use source::{FileBlockRead, FileReadMapping};

use super::{
    AbsoluteBN, BlockIo, Ext4Error, Ext4FileSystem, Ext4Result, ExtentTree, InodeNumber, Jbd2Dev,
};

pub(crate) const MAX_READ_BYTES: usize = super::io::MAX_RUN_IO_BYTES;

/// A bounded snapshot of ordinary-file mappings and cached bytes.
///
/// The caller must prevent writes, truncation, and block reclamation for this
/// inode from preparation through completion. The filesystem/device lock is
/// required while preparing and completing, but not during [`Self::read`].
/// An independent reader must refer to the same device and partition.
#[derive(Debug)]
pub struct PreparedFileRead {
    data: CompletedFileRead,
    reads: Vec<BlockReadSpan>,
    block_size: usize,
}

/// File bytes that have completed all required device reads.
#[derive(Debug)]
pub struct CompletedFileRead {
    inode: InodeNumber,
    bytes: Vec<u8>,
    valid: Range<usize>,
}

#[derive(Debug)]
struct BlockReadSpan {
    physical: AbsoluteBN,
    destination: Range<usize>,
}

impl PreparedFileRead {
    /// Snapshots at most 1 MiB of requested data, plus block alignment padding.
    ///
    /// Returns `None` for larger requests, legacy mappings, or non-regular files; callers should
    /// use the existing serialized read path for those cases. Holes and
    /// unwritten extents remain zero. Cached data overrides queued journal data,
    /// which in turn overrides the backing device, matching `read_run`.
    ///
    /// # Errors
    /// Returns allocation, inode/mapping I/O, invalid-range, or overflow errors.
    pub fn prepare<B: BlockIo>(
        fs: &mut Ext4FileSystem,
        device: &mut Jbd2Dev<B>,
        inode_num: InodeNumber,
        range: Range<u64>,
    ) -> Ext4Result<Option<Self>> {
        let requested = range
            .end
            .checked_sub(range.start)
            .ok_or_else(Ext4Error::invalid_input)?;
        if requested > MAX_READ_BYTES as u64 {
            return Ok(None);
        }
        if requested == 0 {
            return Ok(Some(Self::empty(inode_num, fs.block_size())));
        }
        let inode = fs.get_inode_by_num(device, inode_num)?;
        Self::prepare_with_reader(
            FileReadMapping {
                number: inode_num,
                inode,
                context: crate::ext4::BlockMapContext::from_filesystem(fs),
            },
            &mut MountedFileRead::new(fs, device),
            range,
        )
    }

    pub(crate) fn prepare_with_reader(
        mut mapping: FileReadMapping<'_>,
        reader: &mut impl FileBlockRead,
        range: Range<u64>,
    ) -> Ext4Result<Option<Self>> {
        let requested = range
            .end
            .checked_sub(range.start)
            .ok_or_else(Ext4Error::invalid_input)?;
        if requested > MAX_READ_BYTES as u64 {
            return Ok(None);
        }
        let block_size = mapping.context.block_size();
        let mut plan = Self::empty(mapping.number, block_size);
        if requested == 0 || range.start >= mapping.inode.size() {
            return Ok(Some(plan));
        }
        if !mapping.inode.is_file() || !mapping.inode.uses_extents() {
            return Ok(None);
        }
        let end = range.end.min(mapping.inode.size());
        let first =
            u32::try_from(range.start / block_size as u64).map_err(|_| Ext4Error::overflow())?;
        let last =
            u32::try_from((end - 1) / block_size as u64).map_err(|_| Ext4Error::overflow())?;
        let count = (last - first) as usize + 1;
        let buffer_size = count
            .checked_mul(block_size)
            .ok_or_else(Ext4Error::overflow)?;
        plan.data
            .bytes
            .try_reserve_exact(buffer_size)
            .map_err(|_| Ext4Error::no_memory())?;
        plan.data.bytes.resize(buffer_size, 0);
        plan.reads
            .try_reserve_exact(count)
            .map_err(|_| Ext4Error::no_memory())?;
        let prefix = (range.start % block_size as u64) as usize;
        plan.data.valid = prefix..prefix + (end - range.start) as usize;

        let runs = ExtentTree::with_context(&mut mapping.inode, mapping.context, mapping.number)
            .initialized_runs_with_reader(reader, first, last)?;
        let mut next_logical = u64::from(first);
        for run in runs {
            let run_start = u64::from(run.logical_start);
            let run_end = run_start + u64::from(run.len);
            if run.len == 0 || run_start < next_logical || run_end > u64::from(last) + 1 {
                return Err(Ext4Error::corrupted());
            }
            next_logical = run_end;
            let images = reader.data_images(run.physical_start, run.len)?;
            if images.len() != run.len as usize {
                return Err(Ext4Error::corrupted().with_operation("file:read_image_count"));
            }
            for (index, image) in images.into_iter().enumerate() {
                let relative = run
                    .logical_start
                    .checked_sub(first)
                    .and_then(|start| start.checked_add(index as u32))
                    .ok_or_else(Ext4Error::corrupted)?;
                plan.snapshot_block(
                    image,
                    run.physical_start.checked_add(index as u32)?,
                    relative as usize * block_size,
                )?;
            }
        }
        Ok(Some(plan))
    }

    fn empty(inode_num: InodeNumber, block_size: usize) -> Self {
        Self {
            data: CompletedFileRead {
                inode: inode_num,
                bytes: Vec::new(),
                valid: 0..0,
            },
            reads: Vec::new(),
            block_size,
        }
    }

    /// Reads missing data into privately owned buffers, without accessing fs/dev.
    ///
    /// # Errors
    /// Propagates the independent reader's I/O errors without publishing bytes.
    pub fn read(
        mut self,
        mut read_blocks: impl FnMut(AbsoluteBN, &mut [u8]) -> Ext4Result<()>,
    ) -> Ext4Result<CompletedFileRead> {
        for span in self.reads {
            read_blocks(span.physical, &mut self.data.bytes[span.destination])?;
        }
        Ok(self.data)
    }

    fn snapshot_block(
        &mut self,
        image: Option<Arc<Vec<u8>>>,
        physical: AbsoluteBN,
        start: usize,
    ) -> Ext4Result<()> {
        let block_size = self.block_size;
        let end = start
            .checked_add(block_size)
            .ok_or_else(Ext4Error::corrupted)?;
        let destination = self
            .data
            .bytes
            .get_mut(start..end)
            .ok_or_else(Ext4Error::corrupted)?;
        if let Some(image) = image {
            if image.len() != block_size {
                return Err(Ext4Error::corrupted());
            }
            destination.copy_from_slice(&image);
        } else {
            if let Some(previous) = self.reads.last_mut() {
                let blocks = previous.destination.len() / block_size;
                if previous.destination.end == start
                    && previous.destination.len() < MAX_READ_BYTES
                    && previous.physical.checked_add(blocks as u32)? == physical
                {
                    previous.destination.end = end;
                    return Ok(());
                }
            }
            self.reads.push(BlockReadSpan {
                physical,
                destination: start..end,
            });
        }
        Ok(())
    }
}

impl CompletedFileRead {
    pub(crate) fn inode(&self) -> InodeNumber {
        self.inode
    }

    pub(crate) fn len(&self) -> usize {
        self.valid.len()
    }

    pub(crate) fn copy_to(&self, destination: &mut [u8]) -> Ext4Result<usize> {
        let len = self.valid.len();
        if destination.len() < len {
            return Err(Ext4Error::buffer_too_small(destination.len(), len));
        }
        destination[..len].copy_from_slice(&self.bytes[self.valid.clone()]);
        Ok(len)
    }

    /// Records normal read access and copies only the valid file bytes.
    ///
    /// Call while holding the same inode protection and filesystem lock used
    /// for preparation. Bytes beyond EOF in `destination` remain untouched.
    ///
    /// # Errors
    /// Returns a short-buffer error or propagates the existing atime update error.
    pub fn complete<B: BlockIo>(
        &self,
        fs: &mut Ext4FileSystem,
        device: &mut Jbd2Dev<B>,
        destination: &mut [u8],
    ) -> Ext4Result<usize> {
        let len = self.valid.len();
        if destination.len() < len {
            return Err(Ext4Error::buffer_too_small(destination.len(), len));
        }
        if len != 0 {
            fs.touch_inode_atime_if_needed(device, self.inode)?;
            destination[..len].copy_from_slice(&self.bytes[self.valid.clone()]);
        }
        Ok(len)
    }
}
