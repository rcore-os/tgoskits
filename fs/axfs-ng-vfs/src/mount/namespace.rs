use super::*;

mod retirement;

// Diagnostic numbering only; namespace identity is its actual Arc allocation.
static MOUNT_NAMESPACE_ID: AtomicU64 = AtomicU64::new(1);

/// An independent mount topology with a permanent, stable root anchor.
///
/// Copy the `Arc`, not the entity. The anchor is not necessarily the currently
/// visible root: root coverings and pivot operations remain below it.
/// Final release disconnects only this namespace's remaining topology; retained
/// paths keep their entities and uses. It may wait for the topology gate, so
/// callers must move the last reference out of their own protected regions.
///
/// ```compile_fail,E0599
/// use axfs_ng_vfs::MountNamespace;
/// fn duplicate_entity(namespace: MountNamespace) -> MountNamespace {
///     namespace.clone() // Sharing uses Arc; a new entity needs a tree clone.
/// }
/// ```
#[derive(Debug)]
pub struct MountNamespace {
    id: u64,
    root_mount: Arc<Mountpoint>,
}

/// Single-use creation provenance bound to one exact namespace allocation.
///
/// Native contexts consume it directly; OS adapters consume it to bind their
/// creator. Existing namespace references cannot recreate this credential.
///
/// ```compile_fail,E0382
/// use axfs_ng_vfs::CreatedMountNamespace;
/// fn bind_twice(created: CreatedMountNamespace) {
///     let first = created.into_namespace();
///     let second = created.into_namespace();
/// }
/// ```
#[derive(Debug)]
pub struct CreatedMountNamespace {
    namespace: Arc<MountNamespace>,
}

impl CreatedMountNamespace {
    /// Consumes the unique creation provenance, without allocating.
    pub fn into_namespace(self) -> Arc<MountNamespace> {
        self.namespace
    }
}

impl MountNamespace {
    /// Adopts an unbound parentless tree during ordinary initialization.
    ///
    /// Returns `InvalidInput` for an attached, formerly installed, normally
    /// removed, or previously adopted root. Binding and validation share one
    /// topology transaction.
    /// The role survives this entity and cannot be transferred to another node.
    pub fn from_tree(root: Arc<Mountpoint>) -> VfsResult<CreatedMountNamespace> {
        {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            root.check_topology_live_locked()?;
            if root.location.lock().is_some()
                || root.active_uses.lock().normally_unmounted
                || root.root_role() != MountRootRole::Unbound
            {
                return Err(VfsError::InvalidInput);
            }
            *root.root_role.lock() = MountRootRole::NamespaceAnchor;
        }
        Ok(Self::created(root))
    }

    fn created(root_mount: Arc<Mountpoint>) -> CreatedMountNamespace {
        CreatedMountNamespace {
            namespace: Arc::new(Self {
                id: MOUNT_NAMESPACE_ID.fetch_add(1, Ordering::Relaxed),
                root_mount,
            }),
        }
    }

    /// Returns the kernel-local diagnostic number, not an identity key.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Borrows the stable anchor, including any mounts covered below it.
    pub fn root_mount(&self) -> &Arc<Mountpoint> {
        &self.root_mount
    }

    /// Returns `(mount_id, parent_id, mount)` tuples in DFS order.
    pub fn walk_tree(&self) -> Vec<(u64, u64, Arc<Mountpoint>)> {
        self.root_mount.walk_tree()
    }

    /// Tests actual mount allocation membership, not pathname equivalence.
    pub fn contains_mountpoint(&self, mountpoint: &Arc<Mountpoint>) -> bool {
        self.walk_tree()
            .iter()
            .any(|(_, _, candidate)| Arc::ptr_eq(candidate, mountpoint))
    }

    /// Clones the complete anchored topology and keeps its actual mount map.
    ///
    /// Allocation follows the existing infallible tree-clone model. Retain the
    /// preparation object, then move its retirement out of any caller gates.
    pub fn clone_namespace(self: &Arc<Self>) -> NamespaceClone {
        let mut relations = propagation::RelationRetirement::default();
        let (root, mounts) = {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            let (root, mounts) = self.root_mount.clone_tree_locked(false, &mut relations);
            *root.root_role.lock() = MountRootRole::NamespaceAnchor;
            MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
            (root, mounts)
        };
        NamespaceClone {
            created: Self::created(root),
            retirement: MountCloneRetirement {
                guards: Vec::new(),
                mounts,
                source: self.clone(),
                _relations: relations,
            },
        }
    }

    /// Observes the covering root and acquires its use in one topology boundary.
    ///
    /// This may sleep and is not a CACHED operation. Store the retirement before
    /// inspecting the result; release it only after all caller gates, including
    /// an outer namespace writer, have been released. No pathname lookup occurs.
    pub fn snapshot_root(
        self: &Arc<Self>,
    ) -> (VfsResult<NamespaceRootSnapshot>, MountRootRetirement) {
        let mut retirement = MountRootRetirement {
            locations: vec![self.root_mount.root_location()],
            guard: None,
            _namespace: self.clone(),
        };
        let result = {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            loop {
                let current = retirement.locations.last().expect("anchor is retained");
                let next = current
                    .mountpoint
                    .children
                    .lock()
                    .get(&current.entry.key())
                    .cloned();
                match next {
                    Some(mount) => retirement.locations.push(mount.root_location()),
                    None => break,
                }
            }
            let location = retirement.locations.last().expect("root is retained");
            // Check actual owner before use admission, preserving the PathHandle
            // error order without recursively entering the public acquire gate.
            PathHandle::check_filesystem(location).and_then(|()| {
                let guard = Arc::new(location.mountpoint.acquire_use_locked()?);
                retirement.guard = Some(guard);
                PathHandle::from_mount_use(
                    location,
                    retirement.guard.as_ref().expect("use is retained"),
                )
            })
        };
        let result = result.map(|root| NamespaceRootSnapshot {
            namespace: self.clone(),
            root,
        });
        (result, retirement)
    }
}

/// A complete namespace clone plus the temporary source-to-clone mapping.
#[derive(Debug)]
pub struct NamespaceClone {
    created: CreatedMountNamespace,
    retirement: MountCloneRetirement,
}

impl NamespaceClone {
    /// Borrows the actual source Core for snapshot provenance validation.
    pub fn source_namespace(&self) -> &Arc<MountNamespace> {
        &self.retirement.source
    }

    /// Borrows the new Core without recreating its creation provenance.
    pub fn namespace(&self) -> &Arc<MountNamespace> {
        &self.created.namespace
    }

    /// Transfers an actual mount identity, keeping the original dentry.
    ///
    /// Paths outside the copied tree retain their original use. All paths on
    /// the same new mount share one newly acquired use. The cache owns that
    /// use before any subsequent fallible owner validation.
    pub fn map_path(&mut self, source: &PathHandle) -> VfsResult<PathHandle> {
        let Some((_, target)) = self
            .retirement
            .mounts
            .iter()
            .find(|(old, _)| Arc::ptr_eq(old, source.location().mountpoint()))
        else {
            return Ok(source.clone());
        };
        let guard_index = match self
            .retirement
            .guards
            .iter()
            .position(|guard| Arc::ptr_eq(&guard.mountpoint, target))
        {
            Some(index) => index,
            None => {
                let guard = Arc::new(target.acquire_use()?);
                self.retirement.guards.push(guard);
                self.retirement.guards.len() - 1
            }
        };
        let location = Location::new(target.clone(), source.location().entry().clone());
        PathHandle::from_mount_use(&location, &self.retirement.guards[guard_index])
    }

    /// Separates creation provenance from every temporary owning resource.
    ///
    /// This only moves fields; it does not release the map, source, or uses.
    pub fn into_parts(self) -> (CreatedMountNamespace, MountCloneRetirement) {
        (self.created, self.retirement)
    }
}

/// Opaque clone resources. Drop only after leaving all caller gates.
///
/// No commit action or permanent namespace state is hidden in this value.
#[derive(Debug)]
pub struct MountCloneRetirement {
    guards: Vec<Arc<MountUseGuard>>,
    mounts: MountCloneMap,
    source: Arc<MountNamespace>,
    _relations: propagation::RelationRetirement,
}

/// An exact namespace and the covering root selected by one topology observation.
#[derive(Debug)]
pub struct NamespaceRootSnapshot {
    namespace: Arc<MountNamespace>,
    root: PathHandle,
}

impl NamespaceRootSnapshot {
    /// Borrows the exact target namespace.
    pub fn namespace(&self) -> &Arc<MountNamespace> {
        &self.namespace
    }

    /// Borrows the selected path without another topology observation.
    pub fn root(&self) -> &PathHandle {
        &self.root
    }

    /// Moves the validated pair into a join preparation without allocating.
    pub fn into_parts(self) -> (Arc<MountNamespace>, PathHandle) {
        (self.namespace, self.root)
    }
}

/// Owns successful and failed root-observation resources for outer retirement.
///
/// Locations retire before the use; the namespace remains alive until last.
#[derive(Debug)]
pub struct MountRootRetirement {
    locations: Vec<Location>,
    guard: Option<Arc<MountUseGuard>>,
    _namespace: Arc<MountNamespace>,
}
