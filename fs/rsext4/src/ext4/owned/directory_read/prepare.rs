//! Mounted preparation and validated publication; neither phase reads disk.

use super::*;
use crate::ForkBlockIo;

impl<D, E, O, W> Ext4<D, MountedServices<E, O, W>>
where
    D: BlockIo,
    E: crate::runtime::EntropySource,
    O: Observer,
    W: crate::runtime::Delay,
{
    /// Prepares a lookup of an already retained parent allocation, without
    /// reading its inode table or directory blocks. See `PreparedDirectoryLookup`
    /// for the namespace and mount admission contract across all phases.
    pub fn prepare_directory_lookup(
        &mut self,
        parent: InodeNumber,
    ) -> Ext4Result<DirectoryLookupPreparation<D>>
    where
        D: ForkBlockIo,
    {
        self.ensure_mounted("directory:prepare_lookup")?;
        if let Some(cached) = self.filesystem.inodetable_cache.get_mut(parent) {
            return self.prepare_directory_snapshot(parent, cached.inode);
        }
        Ok(match self.prepare_uncached_live_inode_read(parent)? {
            Some(prepared) => DirectoryLookupPreparation::Parent(prepared),
            None => DirectoryLookupPreparation::Serialized,
        })
    }

    /// Validates a cold parent read and prepares its directory snapshot without
    /// dirty cache eviction. `None` requests a new attempt after invalidation.
    /// A full dirty inode cache need not admit the parent to allow progress.
    pub fn finish_directory_parent_read(
        &mut self,
        completed: CompletedLiveInodeRead,
    ) -> Ext4Result<Option<DirectoryLookupPreparation<D>>>
    where
        D: ForkBlockIo,
    {
        self.finish_live_inode_record(completed)?
            .map(|(number, inode)| self.prepare_directory_snapshot(number, inode))
            .transpose()
    }

    /// Validates the mount and parent version before exposing either a result
    /// or an I/O/parse error. Retain the found child under the same exclusion.
    pub fn finish_directory_lookup(
        &mut self,
        completed: CompletedDirectoryLookup,
    ) -> Ext4Result<DirectoryLookupOutcome> {
        if !self.read_snapshot_is_current(&completed.snapshot)? {
            return Ok(DirectoryLookupOutcome::Retry);
        }
        Ok(match completed.result? {
            Some(number) => DirectoryLookupOutcome::Found(number),
            None => DirectoryLookupOutcome::Missing,
        })
    }

    fn prepare_directory_snapshot(
        &mut self,
        number: InodeNumber,
        inode: Ext4Inode,
    ) -> Ext4Result<DirectoryLookupPreparation<D>>
    where
        D: ForkBlockIo,
    {
        if !inode.is_dir() {
            return Err(Ext4Error::not_dir());
        }
        Ok(match self.prepare_inode_blocks(number, inode)? {
            Some(blocks) => DirectoryLookupPreparation::Lookup(PreparedDirectoryLookup { blocks }),
            None => DirectoryLookupPreparation::Serialized,
        })
    }
}
