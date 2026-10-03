//! Name-cache ownership and publication across lock-free backend calls.

use alloc::{borrow::ToOwned, string::String};
use core::mem;

use hashbrown::HashMap;

use super::{DirEntry, DirNode, VfsResult};

#[derive(Default)]
pub(super) struct DirectoryCache {
    entries: HashMap<String, DirEntry>,
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

        let result = self.ops.lookup(name);
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
            cache.invalidate_generation();
            cache.entries.insert(name, entry)
        } else {
            None
        }
    }

    pub(super) fn remove_cache_after_mutation(&self, name: &str) -> Option<DirEntry> {
        let mut cache = self.cache.lock();
        cache.invalidate_generation();
        cache.entries.remove(name)
    }

    // A backend may change the namespace and then fail during persistence.
    // Preserve existing positive owners, but reject in-flight lookup results.
    pub(super) fn invalidate_lookup_generation(&self) {
        self.cache.lock().invalidate_generation();
    }

    /// Clears cached names and user data without unlinking backing entries.
    ///
    /// Open locations remain valid. In-flight lookups cannot republish entries
    /// observed before this clear and recreate parent/child reference cycles.
    pub fn clear_cached_entries(&self) {
        let children = {
            let mut cache = self.cache.lock();
            cache.invalidate_generation();
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
    fn invalidate_generation(&mut self) {
        // Both the observed generation and publication use the same mutex. A
        // lookup cannot span 2^64 completed namespace changes in practice.
        self.generation = self.generation.wrapping_add(1);
    }

    pub(super) fn remove_renamed(
        &mut self,
        source: &str,
        destination: &str,
    ) -> (Option<DirEntry>, Option<DirEntry>) {
        self.invalidate_generation();
        let source_entry = self.entries.remove(source);
        let destination_entry = if source == destination {
            None
        } else {
            self.entries.remove(destination)
        };
        (source_entry, destination_entry)
    }
}
