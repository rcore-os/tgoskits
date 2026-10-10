//! Canonical tables whose storage is prepared before the raw state guard.

use alloc::vec::Vec;

/// Sorted canonical entries. Insert/remove never allocate or free a table node.
///
/// Finite architectural namespaces reserve their full capacity at construction.
/// Dynamic namespaces replace an empty retired buffer after task-side growth.
pub(super) struct CanonicalMap<K, V> {
    pub(super) entries: Vec<(K, V)>,
}

impl<K: Ord, V> CanonicalMap<K, V> {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    pub(super) fn get(&self, key: &K) -> Option<&V> {
        self.entries
            .binary_search_by(|(stored, _)| stored.cmp(key))
            .ok()
            .map(|index| &self.entries[index].1)
    }

    pub(super) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.entries
            .binary_search_by(|(stored, _)| stored.cmp(key))
            .ok()
            .map(|index| &mut self.entries[index].1)
    }

    pub(super) fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        match self
            .entries
            .binary_search_by(|(stored, _)| stored.cmp(&key))
        {
            Ok(index) => Some(core::mem::replace(&mut self.entries[index].1, value)),
            Err(index) => {
                assert!(
                    self.entries.len() < self.entries.capacity(),
                    "canonical slot must be prepared before publication"
                );
                self.entries.insert(index, (key, value));
                None
            }
        }
    }

    pub(super) fn remove(&mut self, key: &K) -> Option<V> {
        self.entries
            .binary_search_by(|(stored, _)| stored.cmp(key))
            .ok()
            .map(|index| self.entries.remove(index).1)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }

    pub(super) fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.entries.iter_mut().map(|(key, value)| (&*key, value))
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(key, _)| key)
    }

    pub(super) fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    pub(super) fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.entries.iter_mut().map(|(_, value)| value)
    }
}

/// Finite canonical ownership markers, backed by one prepared table.
pub(super) struct CanonicalSet<K>(CanonicalMap<K, ()>);

impl<K: Ord> CanonicalSet<K> {
    pub(super) fn with_capacity(capacity: usize) -> Self {
        Self(CanonicalMap::with_capacity(capacity))
    }

    pub(super) fn insert(&mut self, key: K) -> bool {
        self.0.insert(key, ()).is_none()
    }

    pub(super) fn remove(&mut self, key: &K) -> bool {
        self.0.remove(key).is_some()
    }

    pub(super) fn contains(&self, key: &K) -> bool {
        self.0.contains_key(key)
    }
}
