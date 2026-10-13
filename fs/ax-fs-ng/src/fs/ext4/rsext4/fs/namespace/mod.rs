//! Directory ownership across lock-external reads and namespace mutations.
//!
//! Order: topology -> directory -> mount state. Directory aliases share one
//! gate while any operation holds it. Rename/rmdir exclude topology readers,
//! avoiding speculative preflight lookups or a new ancestor-locking protocol.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use axfs_ng_vfs::VfsResult;
use rsext4::InodeNumber;

use super::{
    Mutex,
    access::{AccessGate, ReadAccess, WriteAccess},
};

pub(super) struct Namespace {
    topology: Arc<AccessGate>,
    directories: Mutex<BTreeMap<InodeNumber, Weak<AccessGate>>>,
}

pub(crate) struct DirectoryLookupGuard {
    // Release in reverse acquisition order, without holding mount state.
    _directory: ReadAccess,
    _topology: ReadAccess,
}

#[derive(Clone, Copy)]
pub(crate) enum NamespaceChange {
    Directory,
    Topology,
}

pub(super) struct NamespaceChangeGuard {
    _access: ChangeAccess,
}

enum ChangeAccess {
    Directory {
        _directory: WriteAccess,
        _topology: ReadAccess,
    },
    Topology {
        _topology: WriteAccess,
    },
}

impl Namespace {
    pub(super) fn new() -> Self {
        Self {
            topology: Arc::new(AccessGate::new()),
            directories: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn lookup(&self, inode: InodeNumber) -> VfsResult<DirectoryLookupGuard> {
        let topology = self.topology.read()?;
        let directory = self.directory(inode).read()?;
        Ok(DirectoryLookupGuard {
            _directory: directory,
            _topology: topology,
        })
    }

    pub(super) fn change(
        &self,
        inode: InodeNumber,
        scope: NamespaceChange,
    ) -> VfsResult<NamespaceChangeGuard> {
        let access = match scope {
            NamespaceChange::Directory => {
                let topology = self.topology.read()?;
                let directory = self.directory(inode).write()?;
                ChangeAccess::Directory {
                    _directory: directory,
                    _topology: topology,
                }
            }
            NamespaceChange::Topology => ChangeAccess::Topology {
                _topology: self.topology.write()?,
            },
        };
        Ok(NamespaceChangeGuard { _access: access })
    }

    fn directory(&self, inode: InodeNumber) -> Arc<AccessGate> {
        let mut directories = self.directories.lock();
        if let Some(gate) = directories.get(&inode).and_then(Weak::upgrade) {
            return gate;
        }
        directories.retain(|_, gate| gate.strong_count() != 0);
        let gate = Arc::new(AccessGate::new());
        directories.insert(inode, Arc::downgrade(&gate));
        gate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_aliases_share_one_directory_gate_and_retire_idle_keys() {
        let namespace = Namespace::new();
        let inode = InodeNumber::new(12).unwrap();
        let first = namespace.directory(inode);
        let alias = namespace.directory(inode);
        assert!(Arc::ptr_eq(&first, &alias));
        let retired = Arc::downgrade(&first);
        drop(first);
        drop(alias);
        assert!(retired.upgrade().is_none());
        let other = InodeNumber::new(13).unwrap();
        let _active = namespace.directory(other);
        assert!(!namespace.directories.lock().contains_key(&inode));
    }

    #[test]
    fn local_change_allows_another_directory_lookup_to_progress() {
        let namespace = Namespace::new();
        let first = InodeNumber::new(12).unwrap();
        let other = InodeNumber::new(13).unwrap();
        let change = namespace.change(first, NamespaceChange::Directory).unwrap();
        let lookup = namespace.lookup(other).unwrap();
        drop(lookup);
        drop(change);
        drop(namespace.change(first, NamespaceChange::Topology).unwrap());
        drop(namespace.lookup(first).unwrap());
    }
}
