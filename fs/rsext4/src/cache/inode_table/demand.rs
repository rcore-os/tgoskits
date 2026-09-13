//! Versioned cold inode loads without dirty-eviction I/O at publication.

use core::sync::atomic::Ordering;

use super::*;

/// Identity and invalidation for a single inode's in-flight table read.
/// It does not pin the inode's allocation; that belongs to the embedding VFS.
pub(crate) struct InodeLoadVersion {
    cache: Arc<SharedInodes>,
    inode: InodeNumber,
    valid: Arc<AtomicBool>,
}

impl InodeCache {
    pub(crate) fn prepare_load(&mut self, inode: InodeNumber) -> InodeLoadVersion {
        self.pending_reads
            .retain(|_, version| version.strong_count() != 0);
        let valid = self
            .pending_reads
            .get(&inode)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let version = Arc::new(AtomicBool::new(true));
                self.pending_reads.insert(inode, Arc::downgrade(&version));
                version
            });
        InodeLoadVersion {
            cache: self.cache.clone(),
            inode,
            valid,
        }
    }

    pub(crate) fn validate_load(&self, version: &InodeLoadVersion) -> Ext4Result<bool> {
        if !Arc::ptr_eq(&self.cache, &version.cache) {
            return Err(Ext4Error::invalid_input().with_operation("inode_cache:foreign_load"));
        }
        // A mutation/rollback publishes invalidation before replacing bytes.
        // The mount owner serializes validation with all subsequent mutation.
        Ok(version.valid.load(Ordering::Acquire))
    }

    /// Uses current canonical bytes first; otherwise installs a validated clean
    /// record without writing any victim. A full dirty cache may decline it.
    pub(crate) fn publish_load(
        &mut self,
        version: &InodeLoadVersion,
        record: CachedInode,
    ) -> Ext4Result<Option<Ext4Inode>> {
        let valid = self.validate_load(version)?;
        if record.inode_num != version.inode {
            return Err(Ext4Error::invalid_input().with_operation("inode_cache:load_identity"));
        }
        if let Some(current) = self.get(version.inode) {
            self.touch(version.inode);
            return Ok(Some(current.inode));
        }
        if !valid {
            return Ok(None);
        }
        let inode = record.inode;
        let mut entries = self.cache.entries.lock();
        if entries.len() >= self.max_entries {
            let victim = entries
                .iter()
                .filter(|(_, cached)| !cached.dirty)
                .min_by_key(|(_, cached)| cached.last_access)
                .map(|(number, _)| *number);
            let Some(victim) = victim else {
                return Ok(Some(inode));
            };
            entries.remove(&victim);
        }
        entries.insert(version.inode, record);
        drop(entries);
        self.touch(version.inode);
        Ok(Some(inode))
    }

    pub(super) fn invalidate_read(&mut self, inode: InodeNumber) {
        if let Some(version) = self.pending_reads.remove(&inode).and_then(|v| v.upgrade()) {
            version.store(false, Ordering::Release);
        }
    }

    pub(super) fn invalidate_all_reads(&mut self) {
        for version in self.pending_reads.values().filter_map(Weak::upgrade) {
            version.store(false, Ordering::Release);
        }
        self.pending_reads.clear();
    }
}

impl core::fmt::Debug for InodeLoadVersion {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("InodeLoadVersion")
            .field("inode", &self.inode)
            .field("valid", &self.valid.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
