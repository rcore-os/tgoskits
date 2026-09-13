//! Lock-external directory traversal with atomic child lifetime acquisition.

use alloc::sync::Arc;

use rsext4::{DirectoryLookupOutcome, DirectoryLookupPreparation, Ext4Result, FileName};

use super::{Ext4Filesystem, InodeLifetime, InodeNumber, read_cache::MountedReadCache};

impl Ext4Filesystem {
    /// Called by the retained parent while holding mount admission and the
    /// directory's namespace read guard. A result becomes a lifetime under the
    /// same mounted critical section that validated it, before namespace
    /// exclusion is released. No bare inode number escapes that boundary.
    pub(super) fn lookup_admitted_child(
        self: &Arc<Self>,
        parent: InodeNumber,
        name: FileName<'_>,
    ) -> Ext4Result<Option<InodeLifetime>> {
        let mut preparation = self.lock().ext4.prepare_directory_lookup(parent)?;
        loop {
            match preparation {
                DirectoryLookupPreparation::Parent(prepared) => {
                    let completed = prepared.execute();
                    if let Some(next) = self.lock().ext4.finish_directory_parent_read(completed)? {
                        preparation = next;
                        continue;
                    }
                }
                DirectoryLookupPreparation::Lookup(prepared) => {
                    let completed = prepared.execute(name, &mut MountedReadCache(self));
                    let mut state = self.lock();
                    match state.ext4.finish_directory_lookup(completed)? {
                        DirectoryLookupOutcome::Found(number) => {
                            return Ok(Some(state.retain_inode(self, number)));
                        }
                        DirectoryLookupOutcome::Missing => return Ok(None),
                        DirectoryLookupOutcome::Retry => {}
                    }
                }
                DirectoryLookupPreparation::Serialized => {
                    let mut state = self.lock();
                    return Ok(state
                        .ext4
                        .lookup_child_number(parent, name)?
                        .map(|number| state.retain_inode(self, number)));
                }
            }
            preparation = self.lock().ext4.prepare_directory_lookup(parent)?;
        }
    }
}
