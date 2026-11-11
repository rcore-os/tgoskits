//! Operation rights for the immutable, already write-admitted backing graph.

use alloc::{boxed::Box, vec::Vec};

use crate::{
    DirEntry, FilesystemOps, Location, Metadata, MutationRetire, NamespaceCompletion,
    NamespaceDeferred, PreparedWrite, VfsError, VfsResult,
};

/// Native snapshots for a following step using the same operation right.
/// Call only with no native driver/gate held; query other backings first.
/// These snapshots never authorize a later namespace modification.
pub enum NamespaceQuery<'a> {
    Metadata(&'a DirEntry),
    Lookup { parent: &'a DirEntry, name: &'a str },
    Xattr { entry: &'a DirEntry, name: &'a [u8] },
}

impl NamespaceQuery<'_> {
    pub fn entry(&self) -> &DirEntry {
        match self {
            Self::Metadata(entry) | Self::Xattr { entry, .. } => entry,
            Self::Lookup { parent, .. } => parent,
        }
    }
}

pub enum NamespaceRead {
    Metadata(Metadata),
    Entry(Option<DirEntry>),
    Xattr(Vec<u8>),
}

/// A backend's existing operation guard. No namespace gate is held by this right.
/// Guards remain thread-local; neither `Send` nor `Sync` is required.
pub trait NamespaceOperation {}
impl<T> NamespaceOperation for T {}

pub(crate) fn owner_address(owner: &dyn FilesystemOps) -> usize {
    owner as *const dyn FilesystemOps as *const () as usize
}

/// One actual filesystem's operation right, bound only by the VFS factory.
/// There is no caller-supplied owner identifier or public constructor.
pub struct NamespaceAdmission<'a> {
    pub(crate) owner: &'a dyn FilesystemOps,
    _operation: Box<dyn NamespaceOperation + 'a>,
}

impl NamespaceAdmission<'_> {
    /// Uses native helpers without implicitly entering operation admission.
    /// Returned and temporary owners retire with the whole outer operation.
    pub fn query(
        &self,
        query: NamespaceQuery<'_>,
        retired: &MutationRetire,
    ) -> VfsResult<NamespaceRead> {
        retired.retire(query.entry().clone());
        self.check_owner(query.entry().filesystem())?;
        if let NamespaceQuery::Lookup { name, .. } = &query {
            crate::path::verify_entry_name(name)?;
        }
        self.owner.query_namespace(self, query, retired)
    }

    /// Looks up one backend component using this existing same-FS admission.
    ///
    /// The result retains the parent's exact mount without following mounts or
    /// links. Like `query`, this must not run under a native driver/backend gate.
    /// All owned inputs and candidates survive failures until outer retirement.
    /// The caller still performs its own search and backing-identity checks.
    pub fn lookup_backing(
        &self,
        parent: &Location,
        name: &str,
        retired: &MutationRetire,
    ) -> VfsResult<Location> {
        retired.retire(parent.clone());
        match self.query(
            NamespaceQuery::Lookup {
                parent: parent.entry(),
                name,
            },
            retired,
        )? {
            NamespaceRead::Entry(Some(entry)) => {
                retired.retire(entry.clone());
                self.check_owner(entry.filesystem())?;
                Ok(Location::new(parent.mountpoint().clone(), entry))
            }
            NamespaceRead::Entry(None) => Err(VfsError::NotFound),
            other => {
                retired.retire(other);
                Err(VfsError::BadState)
            }
        }
    }

    /// Validates the receiver without entering operation admission again.
    pub fn check_owner(&self, owner: &dyn FilesystemOps) -> VfsResult<()> {
        if owner_address(owner) == owner_address(self.owner) {
            Ok(())
        } else {
            Err(VfsError::CrossesDevices)
        }
    }
}

/// Each real owner appears once, acquired in data-address order outside gates.
/// Mount write authority is separate; read-only dependencies enter as well.
/// Keep the borrowed preparation through all explicit deferred finishes.
pub struct NamespaceAdmissions<'a> {
    admissions: Vec<NamespaceAdmission<'a>>,
}

impl<'a> NamespaceAdmissions<'a> {
    pub(crate) fn contains_owner(&self, owner: usize) -> bool {
        self.admissions
            .iter()
            .any(|admission| owner_address(admission.owner) == owner)
    }

    pub fn acquire(write: &'a PreparedWrite) -> VfsResult<Self> {
        Self::acquire_prepared(crate::prepared_write::PreparedView::write(write))
    }

    pub(crate) fn acquire_prepared(
        prepared: crate::prepared_write::PreparedView<'a>,
    ) -> VfsResult<Self> {
        // Complete enumeration, validation and allocation before entering the
        // first FS. The borrowed receivers belong to external mount roots,
        // never to fields of this collection.
        let mut owners = Vec::new();
        for owner in prepared.filesystems() {
            if owner.stack_depth() > 2 {
                return Err(VfsError::InvalidInput);
            }
            owners.try_reserve(1).map_err(|_| VfsError::NoMemory)?;
            owners.push(owner);
        }
        owners.sort_unstable_by_key(|owner| owner_address(*owner));
        owners.dedup_by_key(|owner| owner_address(*owner));
        let mut admissions = Vec::new();
        admissions
            .try_reserve(owners.len())
            .map_err(|_| VfsError::NoMemory)?;
        for owner in owners {
            let operation = match owner.enter_namespace() {
                Ok(operation) => operation,
                Err(error) => {
                    // Release *all* acquired rights before any preparation
                    // owner may disappear, including on a partial failure.
                    drop(admissions);
                    return Err(error);
                }
            };
            admissions.push(NamespaceAdmission {
                owner,
                _operation: operation,
            });
        }
        Ok(Self { admissions })
    }

    /// Borrows an existing same-FS right; never fills a missing entry in a gate.
    pub fn for_location(&self, location: &Location) -> VfsResult<&NamespaceAdmission<'a>> {
        self.for_owner(location.filesystem())
    }

    pub(crate) fn for_owner(
        &self,
        owner: &dyn FilesystemOps,
    ) -> VfsResult<&NamespaceAdmission<'a>> {
        self.admissions
            .iter()
            .find(|admission| admission.check_owner(owner).is_ok())
            .ok_or(VfsError::CrossesDevices)
    }

    /// Phase two, after *all* layers' gates and publication scopes have ended.
    /// Complete every pressure/abort owner before releasing any admission.
    /// A mismatched set is returned intact, including every typed cause and
    /// remaining owner. No completion is consumed before all owners validate.
    /// On success, explicitly drop *all* admissions, then retirement, and only
    /// then call [`NamespaceDeferred::finish`] for the remaining work.
    ///
    /// Mixed operations keep separate typed collections. Process both sets
    /// before dropping any admission, retaining either result on mismatch.
    /// Do not use `?` on the second set: that could drop the first set's
    /// deferred inode owners while admissions are still held. After releasing
    /// every admission and retired owner, finish successful sets and return
    /// mismatched sets with their completion responsibility intact.
    pub fn pressure<T>(
        &self,
        completions: Vec<NamespaceCompletion<T>>,
    ) -> Result<Vec<NamespaceDeferred<T>>, Vec<NamespaceCompletion<T>>> {
        if completions
            .iter()
            .any(|completion| !completion.admitted_by(self))
        {
            return Err(completions);
        }
        Ok(completions
            .into_iter()
            .map(NamespaceCompletion::pressure)
            .collect())
    }
}
