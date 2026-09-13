//! Mapping preparation, independent ordinary-data I/O, and retryable metadata.

use super::{unwritten::PreparedUnwrittenWrite, *};

mod plan;

use plan::FileDataWrites;

/// All fallible mapping preparation precedes transfer to the I/O endpoint.
pub(crate) struct PreparedFileWrite {
    metadata: WriteMetadata,
    writes: FileDataWrites,
}

/// Data I/O executes once; journal-space retries only revisit metadata.
pub(crate) struct CompletedFileWrite {
    metadata: WriteMetadata,
    result: Ext4Result<()>,
}

struct WriteMetadata {
    inode: InodeNumber,
    end: u64,
    unwritten: Option<PreparedUnwrittenWrite>,
}

impl PreparedFileWrite {
    pub(crate) fn prepare<B: BlockIo>(
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
        number: InodeNumber,
        bytes: core::ops::Range<u64>,
    ) -> Ext4Result<Self> {
        let block_bytes = fs.block_size() as u64;
        let start =
            u32::try_from(bytes.start / block_bytes).map_err(|_| Ext4Error::file_too_large())?;
        let last_byte = bytes
            .end
            .checked_sub(1)
            .ok_or_else(Ext4Error::invalid_input)?;
        let last =
            u32::try_from(last_byte / block_bytes).map_err(|_| Ext4Error::file_too_large())?;
        let mut inode = fs.get_inode_by_num(device, number)?;
        let unwritten =
            if extent_write_needs_preparation(device, fs, number, &mut inode, start, last)? {
                Some(PreparedUnwrittenWrite::prepare_detached(
                    device,
                    fs,
                    WriteTarget {
                        number,
                        logical: start..=last,
                    },
                )?)
            } else {
                None
            };
        // Preparation may have split or allocated extents. Use their current
        // canonical mapping, never the inode loaded before that operation.
        let writes = FileDataWrites::prepare(device, fs, number, bytes.clone())?;
        device.discard_detached_data_image()?;
        Ok(Self {
            metadata: WriteMetadata {
                inode: number,
                end: bytes.end,
                unwritten,
            },
            writes,
        })
    }

    pub(crate) fn execute<B: BlockIo>(
        self,
        device: &mut FileDataEndpoint<B>,
        input: &[u8],
    ) -> CompletedFileWrite {
        let result = self.writes.execute(device, input);
        CompletedFileWrite {
            metadata: self.metadata,
            result,
        }
    }

    pub(crate) fn cancel(self) -> CompletedFileWrite {
        CompletedFileWrite {
            metadata: self.metadata,
            result: Err(Ext4Error::busy().with_operation("write:cancelled")),
        }
    }
}

impl CompletedFileWrite {
    pub(crate) fn io_result(&self) -> Ext4Result<()> {
        self.result
    }

    pub(crate) fn finish<B: BlockIo>(
        &mut self,
        device: &mut Jbd2Dev<B>,
        fs: &mut Ext4FileSystem,
    ) -> Ext4Result<()> {
        self.result?;
        device.discard_detached_data_image()?;
        if let Some(unwritten) = &mut self.metadata.unwritten {
            return unwritten.finish_detached(device, fs, self.metadata.end);
        }
        let number = self.metadata.inode;
        let mut inode = fs.get_inode_by_num(device, number)?;
        if self.metadata.end > inode.size() {
            inode.i_size_lo = self.metadata.end as u32;
            inode.i_size_high = (self.metadata.end >> 32) as u32;
        }
        fs.finalize_inode_update(
            device,
            number,
            &mut inode,
            Ext4InodeMetadataUpdate::write_access(),
        )
    }
}
