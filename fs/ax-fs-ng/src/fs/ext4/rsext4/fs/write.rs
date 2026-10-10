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
            state.ext4.prepare_inode_write(inode, offset, input)
        })?;
        match prepared {
            Some(prepared) => {
                let mut completed = PendingWriteCompletion {
                    filesystem: self,
                    receipt: prepared.execute(),
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
