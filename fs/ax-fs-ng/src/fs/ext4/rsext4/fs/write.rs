//! Data writes retain inode/admission ownership but not mount-state exclusion.

use super::*;

/// Progress can fail before metadata publication is retried. Data I/O has
/// already ended, so release its mapping lease on that error path as well.
struct PendingWriteCompletion<'a> {
    filesystem: &'a Ext4Filesystem,
    receipt: rsext4::CompletedInodeWrite,
}

impl Drop for PendingWriteCompletion<'_> {
    fn drop(&mut self) {
        if self.receipt.needs_publication() {
            let result = self
                .filesystem
                .lock()
                .ext4
                .discard_completed_inode_write(&mut self.receipt);
            if let Err(error) = result {
                log::error!("ext4 completed-write cleanup failed: {error}");
            }
        }
    }
}

impl Ext4Filesystem {
    /// The live inode caller holds content exclusion across preparation, data
    /// I/O and all metadata retries. Admission additionally blocks final clean
    /// publication while a receipt remains unaccepted.
    pub(crate) fn write_extent_inode(
        &self,
        inode: InodeNumber,
        offset: u64,
        input: &[u8],
    ) -> rsext4::Ext4Result<()> {
        let _admission = self.admission.enter()?;
        let prepared = self.with_admitted_writeback_progress(|state| {
            state.ext4.prepare_inode_write(inode, offset, input.len())
        })?;
        match prepared {
            Some(prepared) => {
                let mut completed = PendingWriteCompletion {
                    filesystem: self,
                    receipt: prepared.execute(rsext4::FileWriteInput::Resident(input)),
                };
                self.with_admitted_writeback_progress(|state| {
                    state.ext4.finish_inode_write(&mut completed.receipt)
                })
            }
            None => self.with_admitted_writeback_progress(|state| {
                state.ext4.write_inode(inode, offset, input)
            }),
        }
    }
}

impl super::super::inode::Inode {
    /// Retains one mapping lease and completion for the complete description
    /// request. Only input copying releases physical content exclusion; the
    /// core lease still prevents truncate/reclaim during that interval.
    pub(crate) fn write_inode_iter(
        &self,
        source: &mut dyn axfs_ng_vfs::Read,
        length: usize,
        offset: Option<u64>,
    ) -> VfsResult<(usize, u64)> {
        let filesystem = self.fs();
        let _admission = filesystem.admission.enter().map_err(into_vfs_err)?;
        let mut content = Some(self.content_access().write()?);
        let start = match offset {
            Some(offset) => offset,
            None => {
                filesystem
                    .lock()
                    .ext4
                    .inode(self.number())
                    .map_err(into_vfs_err)?
                    .size
            }
        };
        let end = start
            .checked_add(length as u64)
            .ok_or(VfsError::FileTooLarge)?;
        let prepared = filesystem
            .with_admitted_writeback_progress(|state| {
                state.ext4.prepare_inode_write(self.number(), start, length)
            })
            .map_err(into_vfs_err)?;
        let Some(prepared) = prepared else {
            // This is the core's pre-allocation capability decision, never an
            // I/O-error retry. Keep existing legacy/private-cache semantics.
            drop(content.take());
            return axfs_ng_vfs::write_file_iter(self, source, length, offset);
        };
        let mut source_error = None;
        let receipt = {
            let mut copied = 0;
            let mut copy = |offset: usize, output: &mut [u8]| {
                if offset != copied {
                    return Err(
                        rsext4::Ext4Error::invalid_input().with_operation("write:input_order")
                    );
                }
                // File-backed input may fault into this same inode. It uses
                // the shared side while allocation remains protected by the
                // prepared write's mount-owned mapping lease.
                drop(content.take());
                let result = source
                    .read_exact(output)
                    .map_err(crate::io_error_to_vfs_error);
                let acquired = self.content_access().write();
                let result = match acquired {
                    Ok(guard) => {
                        content = Some(guard);
                        result
                    }
                    Err(error) => result.and(Err(error)),
                };
                if let Err(error) = result {
                    source_error = Some(error);
                    return Err(rsext4::Ext4Error::io().with_operation("write:input_copy"));
                }
                copied += output.len();
                Ok(())
            };
            prepared.execute(rsext4::FileWriteInput::Copy(&mut copy))
        };
        let mut completed = PendingWriteCompletion {
            filesystem,
            receipt,
        };
        let result = filesystem.with_admitted_writeback_progress(|state| {
            state.ext4.finish_inode_write(&mut completed.receipt)
        });
        // The core must consume even a failed input receipt to release its
        // mapping lease; its synthetic transport error never replaces the
        // source's original VFS category.
        if let Some(error) = source_error {
            return Err(error);
        }
        result.map_err(into_vfs_err)?;
        self.finish_metadata_change()?;
        Ok((length, end))
    }
}
