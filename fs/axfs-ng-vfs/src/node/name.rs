//! Stable alias identity and atomically observed naming relationships.

use alloc::{collections::BTreeMap, string::String, sync::Arc};
use core::sync::atomic::{AtomicBool, Ordering};

use super::DirEntry;
use crate::{MountObservation, RawSpinLock, VfsError, VfsResult};

struct NamingTopology {
    writers: usize,
    version: u64,
    domains: BTreeMap<usize, DomainPublication>,
}

struct DomainPublication {
    writers: usize,
    version: u64,
}

static NAMING_TOPOLOGY: RawSpinLock<NamingTopology> = RawSpinLock::new(NamingTopology {
    writers: 0,
    version: 0,
    domains: BTreeMap::new(),
});

/// Nonblocking observation invalidated by an overlapping namespace publication.
#[derive(Debug, Clone, Copy)]
pub struct NameObservation(u64);

impl NameObservation {
    pub fn capture() -> VfsResult<Self> {
        let topology = NAMING_TOPOLOGY.try_lock().ok_or(VfsError::WouldBlock)?;
        if topology.writers != 0 || topology.version == u64::MAX {
            return Err(VfsError::WouldBlock);
        }
        Ok(Self(topology.version))
    }

    pub fn validate(self) -> VfsResult<()> {
        let topology = NAMING_TOPOLOGY.try_lock().ok_or(VfsError::WouldBlock)?;
        if topology.writers == 0 && topology.version != u64::MAX && topology.version == self.0 {
            Ok(())
        } else {
            Err(VfsError::WouldBlock)
        }
    }
}

/// A coherent, completed observation of both naming and mount publications.
///
/// Capture before traversal and validate after inspecting relationships. A
/// scoped create transfers this observation into the actual slot command.
#[derive(Clone, Copy, Debug)]
pub struct PathObservation {
    name: NameObservation,
    mount: MountObservation,
}

impl PathObservation {
    pub fn capture() -> VfsResult<Self> {
        let name = NameObservation::capture()?;
        let mount = MountObservation::capture()?;
        name.validate()?;
        Ok(Self { name, mount })
    }

    pub fn validate(self) -> VfsResult<()> {
        self.name.validate()?;
        self.mount.validate()?;
        self.name.validate()
    }

    pub(super) fn begin_publication(self, domain: usize) -> VfsResult<Arc<NamePublication>> {
        // Atomically convert the original naming observation into this slot's
        // publication. Recheck mounts afterwards so the two observations
        // overlap even if a mount writer started between the checks.
        let publication = NamePublication::begin_checked(domain, Some(self.name))?;
        self.mount.validate()?;
        Ok(publication)
    }
}

pub(super) struct NamePublication {
    start_version: u64,
    closed: AtomicBool,
    domain: usize,
}

impl NamePublication {
    pub fn begin(domain: usize) -> Arc<Self> {
        Self::begin_checked(domain, None).expect("unconditional naming publication")
    }

    fn begin_checked(domain: usize, observation: Option<NameObservation>) -> VfsResult<Arc<Self>> {
        let start_version = {
            let mut topology = NAMING_TOPOLOGY.lock();
            if observation.is_some_and(|observation| {
                topology.writers != 0
                    || topology.version == u64::MAX
                    || topology.version != observation.0
            }) {
                return Err(VfsError::WouldBlock);
            }
            topology.writers += 1;
            let domain = topology.domains.entry(domain).or_insert(DomainPublication {
                writers: 0,
                version: 0,
            });
            domain.writers += 1;
            domain.version
        };
        Ok(Arc::new(Self {
            start_version,
            closed: AtomicBool::new(false),
            domain,
        }))
    }

    pub fn close(&self, first: &[DirEntry], second: &[DirEntry]) {
        let mut topology = NAMING_TOPOLOGY.lock();
        // Completion is inspected only under this lock; it supplies the
        // synchronization, while the atomic permits mutation through Arc.
        if self.closed.swap(true, Ordering::Relaxed) {
            return;
        }
        let domain = topology
            .domains
            .get_mut(&self.domain)
            .expect("live namespace publication domain");
        if domain.version == u64::MAX || domain.version != self.start_version {
            // The backend completed while another publication overtook it.
            // Pinned owners are gathered outside this lock; only pure state
            // writes occur here, and all final drops follow unlock.
            for entry in first.iter().chain(second) {
                entry.set_name_state(NameState::Untrusted);
            }
        }
        domain.version = domain.version.saturating_add(1);
        domain.writers -= 1;
        if domain.writers == 0 {
            topology.domains.remove(&self.domain);
        }
        topology.version = topology.version.saturating_add(1);
        topology.writers -= 1;
    }
}

impl Drop for NamePublication {
    fn drop(&mut self) {
        self.close(&[], &[]);
    }
}

/// Identity of one live directory alias, distinct from an inode identity.
///
/// Users of a key must retain its directory entry until the key is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirEntryId(pub(super) usize);

/// Whether an observed naming relationship may be used for traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameState {
    Root,
    Linked,
    Unlinked,
    Untrusted,
}

/// One consistent observation of an alias's parent, name, and linkage.
#[derive(Debug, Clone)]
pub struct NameSnapshot {
    pub(super) parent: Option<DirEntry>,
    pub(super) name: String,
    pub(super) state: NameState,
}

impl NameSnapshot {
    pub fn parent(&self) -> Option<&DirEntry> {
        self.parent.as_ref()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn state(&self) -> NameState {
        self.state
    }
}

/// Initial naming relationship used when constructing a directory entry.
#[derive(Debug)]
pub struct Reference(pub(super) NameSnapshot);

impl Reference {
    /// Constructs a linked alias, or an anonymous unlinked object without a parent.
    pub fn new(parent: Option<DirEntry>, name: String) -> Self {
        let state = if parent.is_some() {
            NameState::Linked
        } else {
            NameState::Unlinked
        };
        Self(NameSnapshot {
            parent,
            name,
            state,
        })
    }

    pub fn root() -> Self {
        Self(NameSnapshot {
            parent: None,
            name: String::new(),
            state: NameState::Root,
        })
    }
}

/// Backend-defined semantic identity for a dynamically materialized node.
///
/// Components must include the backend instance/view and object generation as
/// needed to reject identifier reuse. Identity is independent of authorization
/// and does not permit skipping a dynamic lookup or caching its link target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeIdentity(Arc<[u64]>);

impl NodeIdentity {
    pub fn new(components: impl Into<Arc<[u64]>>) -> Self {
        Self(components.into())
    }
}
