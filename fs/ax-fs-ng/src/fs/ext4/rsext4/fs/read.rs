//! Mapping and data I/O phases protected by mount and per-inode admission.

use rsext4::InodeReadPreparation;

use super::{read_cache::MountedReadCache, *};

impl Ext4Filesystem {
    /// Called only through a retained InodeLifetime. No arbitrary inode-number
    /// lookup may skip the core allocation check through this private boundary.
    pub(super) fn read_live_inode_info(
        &self,
        inode: InodeNumber,
    ) -> rsext4::Ext4Result<rsext4::InodeInfo> {
        if let Some(info) = self.inode_metadata.try_get(inode)? {
            return Ok(info);
        }
        let _admission = self.admission.enter()?;
        self.read_admitted_live_inode_info(inode)
    }

    /// The lookup owner already retains mount admission across child loading;
    /// reentering a closing admission gate would spuriously reject that read.
    pub(super) fn read_admitted_live_inode_info(
        &self,
        inode: InodeNumber,
    ) -> rsext4::Ext4Result<rsext4::InodeInfo> {
        if let Some(info) = self.inode_metadata.try_get(inode)? {
            return Ok(info);
        }
        loop {
            let prepared = self.lock().ext4.prepare_live_inode_read(inode)?;
            match prepared {
                Some(rsext4::LiveInodeRead::Cached(info)) => return Ok(info),
                Some(rsext4::LiveInodeRead::Pending(prepared)) => {
                    let completed = prepared.execute();
                    if let Some(info) = self.lock().ext4.finish_live_inode_read(completed)? {
                        return Ok(info);
                    }
                }
                None => return self.lock().ext4.inode(inode),
            }
        }
    }

    /// The caller retains shared inode content access until completion.
    /// Mount admission, not the mount-state lock, spans the independent I/O;
    /// shutdown cannot finish and a content writer cannot enter in that phase.
    pub(crate) fn read_inode(
        &self,
        inode: InodeNumber,
        offset: u64,
        output: &mut [u8],
    ) -> VfsResult<usize> {
        let _admission = self.admission.enter().map_err(into_vfs_err)?;
        self.read_admitted_inode(inode, offset, output)
            .map_err(into_vfs_err)
    }

    fn read_admitted_inode(
        &self,
        inode: InodeNumber,
        offset: u64,
        output: &mut [u8],
    ) -> rsext4::Ext4Result<usize> {
        let mut preparation = self
            .lock()
            .ext4
            .prepare_inode_read(inode, offset, output.len())?;
        loop {
            match preparation {
                InodeReadPreparation::Inode(prepared) => {
                    let completed = prepared.execute();
                    if let Some(next) =
                        self.lock()
                            .ext4
                            .finish_read_inode_load(completed, offset, output.len())?
                    {
                        preparation = next;
                        continue;
                    }
                }
                InodeReadPreparation::Read(prepared) => {
                    let completed = prepared.execute(&mut MountedReadCache(self));
                    let validated = self.with_admitted_writeback_progress(|state| {
                        state.ext4.finish_inode_read(&completed, output.len())
                    })?;
                    if let Some(bytes) = validated {
                        return bytes.copy_to(output);
                    }
                }
                InodeReadPreparation::Empty => return Ok(0),
                InodeReadPreparation::Serialized => {
                    return self.with_admitted_writeback_progress(|state| {
                        state.ext4.read_inode(inode, offset, output)
                    });
                }
            }
            preparation = self
                .lock()
                .ext4
                .prepare_inode_read(inode, offset, output.len())?;
        }
    }
}
