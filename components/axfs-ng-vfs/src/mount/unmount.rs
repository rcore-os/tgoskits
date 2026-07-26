use hashbrown::HashSet;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmountKind {
    Normal,
    Detach,
}

/// Failure reported while committing a previously validated unmount plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmountCommitError {
    /// The mount topology changed after the plan was created and must be
    /// planned and admitted again.
    TopologyChanged,
    /// A normal unmount target gained a child mount before commit.
    ResourceBusy,
}

impl From<UnmountCommitError> for VfsError {
    fn from(_error: UnmountCommitError) -> Self {
        VfsError::ResourceBusy
    }
}

#[derive(Debug)]
struct UnmountTarget {
    mountpoint: Arc<Mountpoint>,
    location: Location,
}

#[derive(Debug)]
pub struct UnmountPlan {
    kind: UnmountKind,
    topology_version: u64,
    targets: Vec<UnmountTarget>,
}

impl UnmountPlan {
    pub fn targets(&self) -> impl Iterator<Item = &Arc<Mountpoint>> {
        self.targets.iter().map(|target| &target.mountpoint)
    }

    pub fn has_children(&self) -> bool {
        self.targets
            .iter()
            .any(|target| !target.mountpoint.children.lock().is_empty())
    }

    fn revalidate_locked(&self) -> Result<(), UnmountCommitError> {
        if MOUNT_TOPOLOGY_VERSION.load(Ordering::Acquire) != self.topology_version {
            return Err(UnmountCommitError::TopologyChanged);
        }
        self.revalidate_targets_locked()
    }

    fn revalidate_targets_locked(&self) -> Result<(), UnmountCommitError> {
        for target in &self.targets {
            let Some(location) = target.mountpoint.location() else {
                return Err(UnmountCommitError::TopologyChanged);
            };
            if !location.ptr_eq(&target.location)
                || !location
                    .mountpoint
                    .children
                    .lock()
                    .get(&location.entry.key())
                    .is_some_and(|mount| Arc::ptr_eq(mount, &target.mountpoint))
            {
                return Err(UnmountCommitError::TopologyChanged);
            }
        }
        if self.kind == UnmountKind::Normal && self.has_children() {
            return Err(UnmountCommitError::ResourceBusy);
        }
        Ok(())
    }

    fn commit_locked(self) -> Result<(), UnmountCommitError> {
        self.revalidate_locked()?;
        self.detach_targets_locked()
    }

    fn commit_current_locked(self) -> Result<(), UnmountCommitError> {
        self.revalidate_targets_locked()?;
        self.detach_targets_locked()
    }

    fn detach_targets_locked(self) -> Result<(), UnmountCommitError> {
        for target in &self.targets {
            Mountpoint::detach_from_parent_locked(&target.mountpoint)
                .map_err(|_| UnmountCommitError::TopologyChanged)?;
        }
        for target in &self.targets {
            target.mountpoint.leave_propagation_relations_locked();
        }
        MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub fn commit(self) -> Result<(), UnmountCommitError> {
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        self.commit_locked()
    }
}

impl Mountpoint {
    pub fn plan_unmount(self: &Arc<Self>, kind: UnmountKind) -> VfsResult<UnmountPlan> {
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        self.plan_unmount_locked(kind)
    }

    fn plan_unmount_locked(self: &Arc<Self>, kind: UnmountKind) -> VfsResult<UnmountPlan> {
        if self.is_root() {
            return Err(VfsError::InvalidInput);
        }

        let mut visited = HashSet::new();
        let mut mounts = Vec::new();
        Self::collect_corresponding_roots(self, &mut visited, &mut mounts)?;
        match kind {
            UnmountKind::Normal => {}
            UnmountKind::Detach => {
                let roots = mounts;
                visited.clear();
                mounts = Vec::new();
                for root in roots {
                    Self::collect_subtree_child_first(&root, &mut visited, &mut mounts);
                }
            }
        }

        let mut targets = Vec::with_capacity(mounts.len());
        for mountpoint in mounts {
            let location = mountpoint.location().ok_or(VfsError::InvalidInput)?;
            targets.push(UnmountTarget {
                mountpoint,
                location,
            });
        }
        let plan = UnmountPlan {
            kind,
            topology_version: MOUNT_TOPOLOGY_VERSION.load(Ordering::Acquire),
            targets,
        };
        if kind == UnmountKind::Normal && plan.has_children() {
            return Err(VfsError::ResourceBusy);
        }
        Ok(plan)
    }

    fn collect_corresponding_roots(
        mountpoint: &Arc<Self>,
        visited: &mut HashSet<u64>,
        targets: &mut Vec<Arc<Self>>,
    ) -> VfsResult<()> {
        visited.insert(mountpoint.mount_id());
        targets.push(mountpoint.clone());

        let location = mountpoint.location().ok_or(VfsError::InvalidInput)?;
        let source_parent = location.mountpoint().clone();
        let mut visited_parents = HashSet::new();
        let mut receiving_parents = Vec::new();
        Self::collect_receiving_parents(
            &source_parent,
            &mut visited_parents,
            &mut receiving_parents,
        );
        for parent in receiving_parents {
            if let Some(corresponding) = Self::corresponding_child(&parent, &location)?
                && visited.insert(corresponding.mount_id())
            {
                targets.push(corresponding);
            }
        }
        Ok(())
    }

    fn collect_receiving_parents(
        parent: &Arc<Self>,
        visited: &mut HashSet<u64>,
        receivers: &mut Vec<Arc<Self>>,
    ) {
        if !visited.insert(parent.mount_id()) {
            return;
        }
        for receiver in parent.propagation_targets() {
            receivers.push(receiver.clone());
            Self::collect_receiving_parents(&receiver, visited, receivers);
        }
    }

    fn collect_subtree_child_first(
        mountpoint: &Arc<Self>,
        visited: &mut HashSet<u64>,
        targets: &mut Vec<Arc<Self>>,
    ) {
        if !visited.insert(mountpoint.mount_id()) {
            return;
        }
        for child in mountpoint.children() {
            Self::collect_subtree_child_first(&child, visited, targets);
        }
        targets.push(mountpoint.clone());
    }

    /// Lazily detach this mountpoint and its complete propagation subtree.
    pub fn detach(self: &Arc<Self>) -> VfsResult<()> {
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        self.plan_unmount_locked(UnmountKind::Detach)?
            .commit_current_locked()?;
        Ok(())
    }

    fn detach_from_parent_locked(self: &Arc<Self>) -> VfsResult<()> {
        if self.is_root() {
            return Err(VfsError::InvalidInput);
        }
        let Some(location) = self.location.lock().clone() else {
            return Err(VfsError::InvalidInput);
        };
        location
            .mountpoint
            .children
            .lock()
            .remove(&location.entry.key());
        *self.location.lock() = None;
        Ok(())
    }
}
