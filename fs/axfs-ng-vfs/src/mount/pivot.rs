use super::*;

#[cfg(test)]
pub(super) mod test_hook;

/// Owns successful and rejected pivot observations until caller retirement.
///
/// Store this value before inspecting the paired result, then release it after
/// every caller protection, including a sleepable namespace writer. It holds no
/// lock and neither commits nor rolls back topology when dropped. Sharing the
/// input paths shares their existing uses; no additional use is acquired.
#[must_use = "pivot resources must retire outside caller protections"]
pub struct MountPivotRetirement {
    namespace: Option<Arc<MountNamespace>>,
    _paths: Option<[PathHandle; 3]>,
    traversal: propagation::RetainedLocations,
    mounts: Vec<Arc<Mountpoint>>,
    child_maps: Vec<HashMap<DirEntryId, Arc<Mountpoint>>>,
}

impl core::fmt::Debug for MountPivotRetirement {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MountPivotRetirement")
            .finish_non_exhaustive()
    }
}

enum PreparedSlot {
    Replace(Arc<Mountpoint>),
    Insert(Arc<Mountpoint>),
    Grow(usize),
}

impl MountNamespace {
    /// Authorizes and pivots paths in this exact namespace under one topology gate.
    ///
    /// Both roots must be mount roots connected to this namespace's anchor.
    /// Foreign, retired or movable-anchor inputs are rejected without mutation.
    /// Shared-parent rejection precedes membership rejection. This may sleep;
    /// Scope, pin and IRQ-safe short locks must not surround it. The result and
    /// retirement must both escape any outer sleepable writer before retirement.
    pub fn pivot_root(
        self: &Arc<Self>,
        old_root: &PathHandle,
        new_root: &PathHandle,
        put_old: &PathHandle,
    ) -> (VfsResult<()>, MountPivotRetirement) {
        let mut retirement = MountPivotRetirement::raw(
            old_root.location().mountpoint(),
            new_root.location().mountpoint(),
            put_old.location(),
        );
        retirement.namespace = Some(self.clone());
        retirement._paths = Some([old_root.clone(), new_root.clone(), put_old.clone()]);
        #[cfg(test)]
        test_hook::before_lock(old_root.location().mountpoint());
        let result = {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            if !old_root.location().is_root_of_mount() || !new_root.location().is_root_of_mount() {
                Err(VfsError::InvalidInput)
            } else {
                retirement.pivot_locked(
                    old_root.location().mountpoint(),
                    new_root.location().mountpoint(),
                    put_old.location(),
                )
            }
        };
        (result, retirement)
    }
}

impl MountPivotRetirement {
    pub(super) fn raw(old: &Arc<Mountpoint>, new: &Arc<Mountpoint>, put_old: &Location) -> Self {
        let mut traversal = propagation::RetainedLocations::default();
        traversal.push(old.root_location());
        traversal.push(new.root_location());
        traversal.push(put_old.clone());
        Self {
            namespace: None,
            _paths: None,
            traversal,
            mounts: Vec::with_capacity(4),
            child_maps: Vec::new(),
        }
    }

    /// Follow direct mount edges, not Location::parent, which skips coverings.
    fn belongs_locked(&mut self, mut index: usize) -> bool {
        let anchor = self
            .namespace
            .as_ref()
            .expect("Core-aware pivot")
            .root_mount();
        loop {
            let mount = self.traversal.get(index).mountpoint();
            if Arc::ptr_eq(mount, anchor) {
                return true;
            }
            if mount.root_role() == MountRootRole::NamespaceAnchor {
                return false;
            }
            let Some(parent) = mount.location() else {
                return false;
            };
            index = self.traversal.push(parent);
        }
    }

    fn descendant_locked(&mut self, mut index: usize, ancestor: usize) -> bool {
        loop {
            if self
                .traversal
                .get(index)
                .ptr_eq(self.traversal.get(ancestor))
            {
                return true;
            }
            let Some(parent) = self.traversal.parent(index) else {
                return false;
            };
            index = parent;
            #[cfg(test)]
            test_hook::after_parent(self.traversal.get(index));
        }
    }

    pub(super) fn pivot_locked(
        &mut self,
        old: &Arc<Mountpoint>,
        new: &Arc<Mountpoint>,
        put_old: &Location,
    ) -> VfsResult<()> {
        old.check_topology_live_locked()?;
        new.check_topology_live_locked()?;
        put_old.mountpoint.check_topology_live_locked()?;
        if old.root_role() == MountRootRole::NamespaceAnchor
            || new.root_role() == MountRootRole::NamespaceAnchor
            || Arc::ptr_eq(old, new)
            || !Arc::ptr_eq(put_old.mountpoint(), new)
        {
            return Err(VfsError::InvalidInput);
        }
        put_old.check_is_dir()?;
        if put_old.is_mountpoint() {
            return Err(VfsError::ResourceBusy);
        }
        let new_parent = new.location().ok_or(VfsError::InvalidInput)?;
        let new_parent = self.traversal.push(new_parent);
        let old_parent = old.location().map(|parent| self.traversal.push(parent));
        self.traversal
            .get(new_parent)
            .mountpoint
            .check_topology_live_locked()?;
        if let Some(parent) = old_parent {
            self.traversal
                .get(parent)
                .mountpoint
                .check_topology_live_locked()?;
        }
        if put_old.mountpoint().is_shared()
            || self.traversal.get(new_parent).mountpoint().is_shared()
            || old_parent.is_some_and(|parent| self.traversal.get(parent).mountpoint().is_shared())
        {
            return Err(VfsError::InvalidInput);
        }
        if self.namespace.is_some() {
            let old_belongs = self.belongs_locked(0);
            let new_belongs = self.belongs_locked(1);
            #[cfg(test)]
            test_hook::after_membership(old);
            if !old_belongs || !new_belongs {
                return Err(VfsError::InvalidInput);
            }
        }
        if !self.descendant_locked(2, 1) || !self.descendant_locked(1, 0) {
            return Err(VfsError::InvalidInput);
        }

        // No rejection or allocation under node locks remains after preparation.
        let parent_slot = old_parent.map(|parent| self.prepare_slot(parent, new));
        let put_slot = self.prepare_slot(2, old);
        let new_location = old_parent.map(|parent| self.traversal.get(parent).clone());
        let old_location = put_old.clone();
        self.traversal.reserve(2);
        let parent = self.traversal.get(new_parent);
        let removed = parent
            .mountpoint
            .children
            .lock()
            .remove(&parent.entry.key());
        self.mounts.extend(removed);
        if let Some((parent, slot)) = old_parent.zip(parent_slot) {
            self.publish_slot(parent, slot);
        }
        let previous = core::mem::replace(&mut *new.location.lock(), new_location);
        if let Some(previous) = previous {
            self.traversal.push(previous);
        }
        *old.root_role.lock() = MountRootRole::Ordinary;
        let previous = old.location.lock().replace(old_location);
        if let Some(previous) = previous {
            self.traversal.push(previous);
        }
        self.publish_slot(2, put_slot);
        MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn prepare_slot(&mut self, index: usize, child: &Arc<Mountpoint>) -> PreparedSlot {
        let location = self.traversal.get(index);
        let (occupied, count, capacity) = {
            let children = location.mountpoint.children.lock();
            (
                children.contains_key(&location.entry.key()),
                children.len(),
                children.capacity(),
            )
        };
        if occupied {
            return PreparedSlot::Replace(child.clone());
        }
        if count < capacity {
            return PreparedSlot::Insert(child.clone());
        }
        // Geometric growth copies siblings only when capacity is exhausted.
        let mut replacement =
            HashMap::with_capacity(capacity.saturating_mul(2).max(count.saturating_add(1)));
        for (key, mount) in location.mountpoint.children.lock().iter() {
            replacement.insert(*key, mount.clone());
        }
        self.mounts
            .extend(replacement.insert(location.entry.key(), child.clone()));
        let index = self.child_maps.len();
        self.child_maps.push(replacement);
        PreparedSlot::Grow(index)
    }

    fn publish_slot(&mut self, index: usize, slot: PreparedSlot) {
        let location = self.traversal.get(index);
        match slot {
            PreparedSlot::Replace(mut child) => {
                {
                    let mut children = location.mountpoint.children.lock();
                    let slot = children
                        .get_mut(&location.entry.key())
                        .expect("prepared slot is stable under topology exclusion");
                    core::mem::swap(slot, &mut child);
                }
                self.mounts.push(child);
            }
            PreparedSlot::Insert(child) => {
                let displaced = location
                    .mountpoint
                    .children
                    .lock()
                    .insert(location.entry.key(), child);
                self.mounts.extend(displaced);
            }
            PreparedSlot::Grow(index) => {
                core::mem::swap(
                    &mut *location.mountpoint.children.lock(),
                    &mut self.child_maps[index],
                );
            }
        }
    }
}
