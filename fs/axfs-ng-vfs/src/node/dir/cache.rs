//! Name-cache ownership and publication across lock-free backend calls.

use alloc::{borrow::ToOwned, string::String};
use core::mem;

use hashbrown::{HashMap, HashSet};

use super::{DirEntry, DirNode, VfsError, VfsResult};

// Bound attacker-controlled missing names without evicting live positive
// entries, whose user_data may own the only dirty file-page cache.
const MAX_NEGATIVE_ENTRIES: usize = 128;

#[derive(Default)]
pub(super) struct DirectoryCache {
    entries: HashMap<String, DirEntry>,
    missing: HashSet<String>,
    missing_version: Option<u64>,
    generation: u64,
}

impl DirNode {
    pub(super) fn lookup_and_cache(&self, name: &str) -> VfsResult<DirEntry> {
        if !self.ops.is_cacheable_child(name) {
            return self.ops.lookup(name);
        }

        let generation = {
            let cache = self.cache.lock();
            if let Some(entry) = cache.entries.get(name) {
                return Ok(entry.clone());
            }
            cache.generation
        };

        let version = self.ops.negative_cache_generation()?;
        if let Some(version) = version {
            let cache = self.cache.lock();
            if cache.generation == generation
                && cache.missing_version == Some(version)
                && cache.missing.contains(name)
            {
                return Err(VfsError::NotFound);
            }
        }

        let result = self.ops.lookup(name);
        // Recheck aliases and transaction visibility without a cache guard.
        // Unavailable versions cannot certify the namespace seen by lookup.
        let missing_version = if matches!(result, Err(VfsError::NotFound))
            && version.is_some()
            && self.ops.negative_cache_generation()? == version
        {
            version
        } else {
            None
        };
        let mut cache = self.cache.lock();
        if cache.generation != generation {
            return result;
        }
        match &result {
            Ok(entry) => {
                // Preserve the winning lookup's shared user_data. Keep the
                // losing result alive until after the cache guard is dropped.
                let cached = cache
                    .entries
                    .entry(name.to_owned())
                    .or_insert_with(|| entry.clone())
                    .clone();
                drop(cache);
                Ok(cached)
            }
            Err(VfsError::NotFound) if missing_version.is_some() => {
                cache.remember_missing(name, missing_version);
                result
            }
            Err(_) => result,
        }
    }

    /// Looks up a positive directory entry in the cache.
    pub fn lookup_cache(&self, name: &str) -> Option<DirEntry> {
        if self.ops.is_cacheable_child(name) {
            self.cache.lock().entries.get(name).cloned()
        } else {
            None
        }
    }

    /// Publishes an entry and invalidates observations preceding this mutation.
    ///
    /// The replaced entry is returned so its resources are released outside the
    /// cache lock. This does not change the backing filesystem.
    pub fn insert_cache(&self, name: String, entry: DirEntry) -> Option<DirEntry> {
        if self.ops.is_cacheable_child(&name) {
            let mut cache = self.cache.lock();
            cache.invalidate_missing();
            cache.entries.insert(name, entry)
        } else {
            None
        }
    }

    pub(super) fn remove_cache_after_mutation(&self, name: &str) -> Option<DirEntry> {
        let mut cache = self.cache.lock();
        cache.invalidate_missing();
        cache.entries.remove(name)
    }

    // A backend may change the namespace and then fail during persistence.
    // Preserve existing positive owners, but never retain pre-operation misses.
    pub(super) fn invalidate_missing_entries(&self) {
        self.cache.lock().invalidate_missing();
    }

    /// Clears cached names and user data without unlinking backing entries.
    ///
    /// Open locations remain valid. In-flight lookups cannot republish entries
    /// observed before this clear and recreate parent/child reference cycles.
    pub fn clear_cached_entries(&self) {
        let children = {
            let mut cache = self.cache.lock();
            cache.invalidate_missing();
            mem::take(&mut cache.entries)
        };
        for (_, child) in children {
            if let Ok(dir) = child.as_dir() {
                dir.clear_cached_entries();
            }
        }
    }
}

impl DirectoryCache {
    fn remember_missing(&mut self, name: &str, version: Option<u64>) {
        if self.missing.len() == MAX_NEGATIVE_ENTRIES || self.missing_version != version {
            self.missing.clear();
        }
        self.missing_version = version;
        self.missing.insert(name.to_owned());
    }

    fn invalidate_missing(&mut self) {
        self.missing.clear();
        self.missing_version = None;
        // Both the observed version and publication use the same mutex. A
        // lookup cannot span 2^64 completed namespace changes in practice.
        self.generation = self.generation.wrapping_add(1);
    }

    pub(super) fn remove_renamed(
        &mut self,
        source: &str,
        destination: &str,
    ) -> (Option<DirEntry>, Option<DirEntry>) {
        self.invalidate_missing();
        let source_entry = self.entries.remove(source);
        let destination_entry = if source == destination {
            None
        } else {
            self.entries.remove(destination)
        };
        (source_entry, destination_entry)
    }
}
