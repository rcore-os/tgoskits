//! Weak alias indexing and bounded, independently owned positive retention.

use alloc::{borrow::ToOwned, collections::VecDeque, string::String, sync::Arc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use super::{DirEntry, DirNode, VfsError, VfsResult};
use crate::{MutationRetire, NameState, RawSpinLock, WeakDirEntry};

const MAX_NEGATIVE_ENTRIES: usize = 128;
const MAX_RETAINED_ENTRIES: usize = 256;

// No filesystem, directory, or mount owns this cache: retained children cannot
// lead back to the retention owner through their strong ancestor chains.
static RETAINED_ENTRIES: RawSpinLock<VecDeque<DirEntry>> = RawSpinLock::new(VecDeque::new());

#[derive(Default)]
pub(super) struct DirectoryCache {
    entries: HashMap<String, WeakDirEntry>,
    valid: HashSet<String>,
    missing: HashSet<String>,
    missing_version: Option<u64>,
    generation: u64,
    namespace_generation: u64,
    writers: usize,
    publications: usize,
}

pub(super) struct NameChange<'a> {
    directory: &'a DirNode,
    complete: bool,
    publication: Arc<super::super::name::NamePublication>,
    affected: Vec<DirEntry>,
    retired: Option<&'a MutationRetire>,
}

impl NameChange<'_> {
    fn collect_affected(&mut self) {
        self.affected.extend(
            self.directory
                .cache
                .lock()
                .entries
                .values()
                .filter_map(WeakDirEntry::upgrade),
        );
    }

    pub(super) fn finish(&mut self) {
        self.collect_affected();
        self.publication.close(&self.affected, &[]);
        self.complete = true;
    }

    pub(super) fn invalidate_affected(&mut self) {
        self.collect_affected();
        self.directory.cache.lock().valid.clear();
        for entry in &self.affected {
            entry.invalidate_name();
        }
    }

    pub(super) fn finish_pair(&mut self, other: &mut Self) {
        self.collect_affected();
        other.collect_affected();
        self.publication.close(&self.affected, &other.affected);
        self.complete = true;
        other.complete = true;
    }
}

impl Drop for NameChange<'_> {
    fn drop(&mut self) {
        let uncertain = {
            let mut cache = self.directory.cache.lock();
            cache.writers -= 1;
            cache.namespace_generation = cache.namespace_generation.wrapping_add(1);
            cache.invalidate_missing();
            if self.complete {
                Vec::new()
            } else {
                cache.valid.clear();
                cache
                    .entries
                    .values()
                    .filter_map(WeakDirEntry::upgrade)
                    .collect()
            }
        };
        for entry in &uncertain {
            entry.set_name_state(NameState::Untrusted);
        }
        if !self.complete {
            for entry in &self.affected {
                entry.set_name_state(NameState::Untrusted);
            }
            if Arc::strong_count(&self.publication) == 1 {
                self.publication.close(&self.affected, &uncertain);
            }
        }
        if let Some(retired) = self.retired {
            retired.retire(core::mem::take(&mut self.affected));
            retired.retire(uncertain);
        }
    }
}

fn retain_deferred(entry: &DirEntry) -> (Option<DirEntry>, Option<DirEntry>) {
    {
        let mut entries = RETAINED_ENTRIES.lock();
        let duplicate = entries
            .iter()
            .rposition(|item| item.ptr_eq(entry))
            .and_then(|index| entries.remove(index));
        entries.push_back(entry.clone());
        let evicted = if entries.len() > MAX_RETAINED_ENTRIES {
            entries.pop_front()
        } else {
            None
        };
        (duplicate, evicted)
    }
}

fn retain(entry: &DirEntry) {
    drop(retain_deferred(entry));
}

fn release_retained(children: &[DirEntry]) {
    let retired = {
        let mut entries = RETAINED_ENTRIES.lock();
        let mut retired = Vec::new();
        let mut index = 0;
        while index < entries.len() {
            if children.iter().any(|child| child.ptr_eq(&entries[index])) {
                if let Some(entry) = entries.remove(index) {
                    retired.push(entry);
                }
            } else {
                index += 1;
            }
        }
        retired
    };
    drop(retired);
}

impl DirNode {
    pub(super) fn begin_name_change(&self) -> NameChange<'_> {
        let publication = super::super::name::NamePublication::begin(self.ops.namespace_domain());
        self.begin_name_change_with(publication)
    }

    fn begin_name_change_with(
        &self,
        publication: Arc<super::super::name::NamePublication>,
    ) -> NameChange<'_> {
        let affected;
        {
            let mut cache = self.cache.lock();
            cache.writers += 1;
            cache.namespace_generation = cache.namespace_generation.wrapping_add(1);
            cache.invalidate_missing();
            affected = cache
                .entries
                .values()
                .filter_map(WeakDirEntry::upgrade)
                .collect();
        }
        NameChange {
            directory: self,
            complete: false,
            publication,
            affected,
            retired: None,
        }
    }

    pub(super) fn begin_locked_name_change<'a>(
        &'a self,
        domain: usize,
        retired: &'a MutationRetire,
    ) -> NameChange<'a> {
        let mut change =
            self.begin_name_change_with(super::super::name::NamePublication::begin(domain));
        change.retired = Some(retired);
        change
    }

    pub(super) fn begin_related_locked_name_change<'a>(
        &'a self,
        other: &NameChange<'_>,
        retired: &'a MutationRetire,
    ) -> NameChange<'a> {
        let mut change = self.begin_name_change_with(other.publication.clone());
        change.retired = Some(retired);
        change
    }

    pub(super) fn canonicalize_locked(
        &self,
        name: &str,
        candidate: DirEntry,
        cacheable: bool,
        retired: &MutationRetire,
    ) -> DirEntry {
        retired.retire(candidate.clone());
        let existing = self
            .cache
            .lock()
            .entries
            .get(name)
            .and_then(WeakDirEntry::upgrade);
        let winner = match existing {
            Some(existing) => {
                let same = existing.same_backend_object(&candidate);
                retired.retire(existing.clone());
                if !same {
                    existing.set_name_state(NameState::Untrusted);
                }
                if same { existing } else { candidate }
            }
            None => candidate,
        };
        winner.set_name_state(NameState::Linked);
        self.insert_locked_cache(name.to_owned(), &winner, cacheable, retired);
        winner
    }

    pub(super) fn insert_locked_cache(
        &self,
        name: String,
        entry: &DirEntry,
        cacheable: bool,
        retired: &MutationRetire,
    ) {
        let replaced = {
            let mut cache = self.cache.lock();
            cache.invalidate_missing();
            cache.prune_dead_names();
            if cacheable {
                cache.valid.insert(name.clone());
            }
            cache
                .entries
                .insert(name, entry.downgrade())
                .and_then(|old| old.upgrade())
        };
        retired.retire(replaced);
        if cacheable {
            retired.retire(retain_deferred(entry));
        }
    }

    pub(super) fn locked_entry(&self, name: &str) -> Option<DirEntry> {
        self.cache
            .lock()
            .entries
            .get(name)
            .and_then(WeakDirEntry::upgrade)
    }

    pub(super) fn invalidate_locked_name(&self, name: &str, retired: &MutationRetire) {
        let removed = self.remove_cache_after_mutation(name);
        if let Some(entry) = &removed {
            entry.set_name_state(NameState::Untrusted);
        }
        retired.retire(removed);
    }

    pub(super) fn release_mutation_probe(entry: DirEntry) -> Option<DirEntry> {
        release_retained(core::slice::from_ref(&entry));
        entry.has_other_owners().then_some(entry)
    }

    pub(super) fn lookup_and_cache(&self, name: &str) -> VfsResult<DirEntry> {
        let cacheable = self.ops.is_cacheable_child(name);
        if let Some(entry) = self.lookup_cache(name) {
            retain(&entry);
            return Ok(entry);
        }
        let (generation, namespace_generation) = {
            let cache = self.cache.lock();
            (cache.generation, cache.namespace_generation)
        };
        let version = if cacheable {
            self.ops.negative_cache_generation()?
        } else {
            None
        };
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
        let missing_version = if matches!(result, Err(VfsError::NotFound))
            && version.is_some()
            && self.ops.negative_cache_generation()? == version
        {
            version
        } else {
            None
        };
        match result {
            Ok(candidate) => {
                // Dynamic entries still execute the backend lookup above. Their
                // supplied semantic identity only canonicalizes the live alias.
                if !cacheable && candidate.identity().is_none() {
                    return Ok(candidate);
                }
                self.publish_lookup(name, candidate, generation, namespace_generation, cacheable)
            }
            Err(error) => {
                let mut cache = self.cache.lock();
                if cache.generation == generation && missing_version.is_some() {
                    cache.remember_missing(name, missing_version);
                }
                Err(error)
            }
        }
    }

    fn publish_lookup(
        &self,
        name: &str,
        candidate: DirEntry,
        generation: u64,
        namespace_generation: u64,
        cacheable: bool,
    ) -> VfsResult<DirEntry> {
        let canonical = self.ops.canonical_name(name, &candidate);
        let candidate_state = candidate.name_state();
        let (winner, previous, retain_winner) = {
            let mut cache = self.cache.lock();
            if cache.namespace_generation != namespace_generation || cache.writers != 0 {
                return Err(VfsError::WouldBlock);
            }
            let existing = cache
                .entries
                .get(&canonical)
                .and_then(WeakDirEntry::upgrade);
            let winner = match &existing {
                Some(existing) if existing.same_backend_object(&candidate) => {
                    if candidate_state == NameState::Linked {
                        // Pure state publication under cache exclusion; no
                        // parent owner or backend resource is released here.
                        existing.set_name_state(NameState::Linked);
                    }
                    existing.clone()
                }
                _ => candidate.clone(),
            };
            cache.prune_dead_names();
            cache.entries.insert(canonical.clone(), winner.downgrade());
            cache.entries.insert(name.to_owned(), winner.downgrade());
            let retain_winner = cacheable && cache.generation == generation;
            if retain_winner {
                cache.valid.insert(canonical);
                cache.valid.insert(name.to_owned());
            }
            (winner, existing, retain_winner)
        };
        drop(previous);
        if retain_winner {
            retain(&winner);
        }
        Ok(winner)
    }

    pub fn lookup_cache(&self, name: &str) -> Option<DirEntry> {
        if !self.ops.is_cacheable_child(name) {
            return None;
        }
        let entry = {
            let cache = self.cache.lock();
            (cache.writers == 0 && cache.valid.contains(name))
                .then(|| cache.entries.get(name).and_then(WeakDirEntry::upgrade))
                .flatten()
        }?;
        matches!(entry.name_state(), NameState::Root | NameState::Linked).then_some(entry)
    }

    /// Publishes a committed alias. Replaced owners are returned for lock-external destruction.
    pub fn insert_cache(&self, name: String, entry: DirEntry) -> Option<DirEntry> {
        let cacheable = self.ops.is_cacheable_child(&name);
        let replaced = {
            let mut cache = self.cache.lock();
            cache.invalidate_missing();
            cache.prune_dead_names();
            if cacheable {
                cache.valid.insert(name.clone());
            }
            cache
                .entries
                .insert(name, entry.downgrade())
                .and_then(|old| old.upgrade())
        };
        if cacheable {
            retain(&entry);
        }
        replaced
    }

    pub(super) fn remove_cache_after_mutation(&self, name: &str) -> Option<DirEntry> {
        let mut cache = self.cache.lock();
        cache.invalidate_missing();
        let old = cache.entries.remove(name)?;
        cache.entries.retain(|_, entry| !entry.ptr_eq(&old));
        let DirectoryCache { valid, entries, .. } = &mut **cache;
        valid.retain(|name| entries.contains_key(name));
        old.upgrade()
    }

    pub(super) fn invalidate_missing_entries(&self) {
        self.cache.lock().invalidate_missing();
    }

    /// Releases retention without discarding the identity of an active alias.
    pub fn clear_cached_entries(&self) {
        let children: Vec<_> = {
            let mut cache = self.cache.lock();
            cache.invalidate_missing();
            cache.valid.clear();
            cache
                .entries
                .values()
                .filter_map(WeakDirEntry::upgrade)
                .collect()
        };
        for child in &children {
            if let Ok(dir) = child.as_dir() {
                dir.clear_cached_entries();
            }
        }
        release_retained(&children);
    }
}

impl DirectoryCache {
    fn prune_dead_names(&mut self) {
        self.publications = self.publications.wrapping_add(1);
        if self.publications.is_multiple_of(64) {
            self.entries.retain(|_, entry| entry.is_alive());
            let Self { valid, entries, .. } = self;
            valid.retain(|name| entries.contains_key(name));
        }
    }

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
        self.generation = self.generation.wrapping_add(1);
    }
}
