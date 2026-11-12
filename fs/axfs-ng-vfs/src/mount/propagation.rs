use hashbrown::HashSet;
use log::warn;

use super::*;

mod relations;
pub(super) use relations::RelationRetirement;

/// Callers declare this before taking topology exclusion. Even receivers
/// rejected as retired retain their successful Weak upgrades until gate exit.
#[derive(Default)]
pub(super) struct PropagationRetirement {
    locations: RetainedLocations,
    source_path: RetainedLocations,
    receivers: Vec<Arc<Mountpoint>>,
    mounts: Vec<Arc<Mountpoint>>,
    child_maps: Vec<HashMap<DirEntryId, Arc<Mountpoint>>>,
    relations: RelationRetirement,
}

enum PreparedChildSlot {
    Replace(Arc<Mountpoint>),
    Insert(Arc<Mountpoint>),
    Grow(usize),
}

/// Traversal owns each observation before following another edge. In
/// particular, a name snapshot can own a parent no longer held by its alias.
#[derive(Default)]
pub(super) struct RetainedLocations {
    locations: Vec<Location>,
    snapshots: Vec<crate::NameSnapshot>,
}

impl RetainedLocations {
    pub(super) fn push(&mut self, location: Location) -> usize {
        let index = self.locations.len();
        self.locations.push(location);
        index
    }

    pub(super) fn get(&self, index: usize) -> &Location {
        &self.locations[index]
    }

    /// Same parent step as `Location::parent`, retaining covered locations and
    /// naming observations that the ordinary convenience traversal discards.
    pub(super) fn parent(&mut self, mut index: usize) -> Option<usize> {
        loop {
            let current = self.get(index);
            if current.is_root_of_mount() {
                let covered = current.mountpoint.location()?;
                index = self.push(covered);
            } else {
                let snapshot = current.entry.name_snapshot();
                self.snapshots.push(snapshot);
                let parent = self.snapshots.last()?.parent()?.clone();
                let parent = self.get(index).wrap(parent);
                return Some(self.push(parent));
            }
        }
    }

    fn lookup(&mut self, index: usize, name: &str) -> VfsResult<usize> {
        match name {
            DOT => Ok(index),
            DOTDOT => Ok(self.parent(index).unwrap_or(index)),
            _ => {
                let current = self.get(index);
                let entry = current.entry.as_dir()?.lookup(name)?;
                let location = current.wrap(entry);
                let mut index = self.push(location);
                loop {
                    let current = self.get(index);
                    let child = current
                        .mountpoint
                        .children
                        .lock()
                        .get(&current.entry.key())
                        .cloned();
                    let Some(child) = child else {
                        return Ok(index);
                    };
                    index = self.push(child.root_location());
                }
            }
        }
    }
}

impl PropagationRetirement {
    fn topmost_root_locked(&mut self, child: &Arc<Mountpoint>) -> VfsResult<usize> {
        let mut index = self.locations.push(child.root_location());
        loop {
            let root = self.locations.get(index);
            root.mountpoint.check_topology_live_locked()?;
            if root.mountpoint.root_role() == MountRootRole::NamespaceAnchor {
                return Err(VfsError::InvalidInput);
            }
            let covering = root
                .mountpoint
                .children
                .lock()
                .get(&root.entry.key())
                .cloned();
            let Some(covering) = covering else {
                return Ok(index);
            };
            index = self.locations.push(covering.root_location());
        }
    }

    /// Topology exclusion keeps the key and available capacity stable through
    /// commit. Only growth snapshots sibling owners, with geometric capacity.
    fn prepare_child_slot(
        &mut self,
        location: &Location,
        child: &Arc<Mountpoint>,
    ) -> PreparedChildSlot {
        let (occupied, count, capacity) = {
            let children = location.mountpoint.children.lock();
            (
                children.contains_key(&location.entry.key()),
                children.len(),
                children.capacity(),
            )
        };
        if occupied {
            return PreparedChildSlot::Replace(child.clone());
        }
        if count < capacity {
            return PreparedChildSlot::Insert(child.clone());
        }

        // Allocate before taking the node lock. Doubling makes a sequence of
        // distinct-slot insertions amortized linear in copied sibling owners.
        let capacity = capacity.saturating_mul(2).max(count.saturating_add(1));
        let index = self.child_maps.len();
        self.child_maps.push(HashMap::with_capacity(capacity));
        let replacement = &mut self.child_maps[index];
        for (key, mount) in location.mountpoint.children.lock().iter() {
            replacement.insert(*key, mount.clone());
        }
        if let Some(displaced) = replacement.insert(location.entry.key(), child.clone()) {
            self.mounts.push(displaced);
        }
        PreparedChildSlot::Grow(index)
    }

    /// Prepared capacity and retirement slots make every branch allocation-free.
    /// Replaced owners leave the node lock before entering the carrier.
    fn publish_child_slot_locked(&mut self, location: &Location, prepared: PreparedChildSlot) {
        match prepared {
            PreparedChildSlot::Replace(mut child) => {
                {
                    let mut children = location.mountpoint.children.lock();
                    let slot = children
                        .get_mut(&location.entry.key())
                        .expect("prepared occupied slot is stable under topology exclusion");
                    core::mem::swap(slot, &mut child);
                }
                self.mounts.push(child);
            }
            PreparedChildSlot::Insert(child) => {
                let displaced = location
                    .mountpoint
                    .children
                    .lock()
                    .insert(location.entry.key(), child);
                if let Some(displaced) = displaced {
                    self.mounts.push(displaced);
                }
            }
            PreparedChildSlot::Grow(index) => {
                core::mem::swap(
                    &mut *location.mountpoint.children.lock(),
                    &mut self.child_maps[index],
                );
            }
        }
    }

    #[cfg(test)]
    pub(super) fn has_receiver(&self, receiver: &Weak<Mountpoint>) -> bool {
        self.receivers
            .iter()
            .any(|mount| Arc::as_ptr(mount) == receiver.as_ptr())
    }

    fn collect_receivers_locked(&mut self, source: &Mountpoint) {
        for relations in [&source.peers, &source.slaves] {
            let count = relations.lock().len();
            self.receivers.reserve(count);
            for relation in relations.lock().iter() {
                if let Some(mount) = relation.upgrade() {
                    self.receivers.push(mount);
                }
            }
        }
    }

    fn resolve_locked(
        &mut self,
        target: &Arc<Mountpoint>,
        components: &[String],
    ) -> VfsResult<Location> {
        let mut index = self.locations.push(target.root_location());
        for component in components {
            index = self.locations.lookup(index, component)?;
        }
        Ok(self.locations.get(index).clone())
    }
}

impl Mountpoint {
    fn relative_path_from_mount_root(
        location: &Location,
        mut retirement: Option<&mut RetainedLocations>,
    ) -> VfsResult<Vec<String>> {
        let mut components = Vec::new();
        let mut current = location.clone();
        if let Some(retirement) = retirement.as_mut() {
            retirement.push(current.clone());
        }
        while !current.is_root_of_mount() {
            let snapshot = current.entry.name_snapshot();
            if let Some(retirement) = retirement.as_mut() {
                retirement.snapshots.push(snapshot.clone());
            }
            if snapshot.state() == crate::NameState::Untrusted {
                return Err(VfsError::WouldBlock);
            }
            components.push(snapshot.name().to_owned());
            current = current.wrap(snapshot.parent().cloned().ok_or(VfsError::InvalidInput)?);
            if let Some(retirement) = retirement.as_mut() {
                retirement.push(current.clone());
            }
        }
        components.reverse();
        Ok(components)
    }

    pub(super) fn corresponding_child(
        parent: &Arc<Self>,
        source_location: &Location,
    ) -> VfsResult<Option<Arc<Self>>> {
        let source_path = Self::relative_path_from_mount_root(source_location, None)?;
        for child in parent.children() {
            let Some(location) = child.location() else {
                continue;
            };
            if Self::relative_path_from_mount_root(&location, None)? == source_path {
                return Ok(Some(child));
            }
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(super) fn rebuild_cloned_relations(clones: &[(Arc<Self>, Arc<Self>)]) {
        let mut retirement = RelationRetirement::default();
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        Self::rebuild_cloned_relations_locked(clones, &mut retirement);
    }

    pub(super) fn rebuild_cloned_relations_locked(
        clones: &[(Arc<Self>, Arc<Self>)],
        retirement: &mut RelationRetirement,
    ) {
        for (source, cloned) in clones {
            if source.is_shared() {
                cloned.join_shared_group_locked(source, retirement);
            }
        }

        // Joining a shared group clears the clone's previous propagation
        // relations. Rebuild slave edges only after every shared clone has
        // joined its group so traversal order cannot remove an attached slave.
        for (source, cloned) in clones {
            if !source.is_slave() {
                continue;
            }
            let masters = retirement.collect_locked(&source.masters);
            #[cfg(test)]
            clone_relation_hook::after_upgrade(
                source,
                clone_relation_hook::UpgradeKind::SlaveMasters,
                retirement.slice(masters.clone()),
            );
            for index in masters {
                let master = retirement.owner(index);
                let cloned_master = clones
                    .iter()
                    .find(|(candidate, _)| Arc::ptr_eq(candidate, master))
                    .map(|(_, clone)| clone)
                    .unwrap_or(master);
                Self::attach_master_locked(cloned, cloned_master);
            }
        }
    }

    pub fn is_shared(&self) -> bool {
        self.propagation() == PropagationType::Shared
    }

    pub fn is_slave(&self) -> bool {
        self.propagation() == PropagationType::Slave
    }

    pub fn is_unbindable(&self) -> bool {
        self.propagation() == PropagationType::Unbindable
    }

    pub(super) fn leave_propagation_relations_locked(
        self: &Arc<Self>,
        retirement: &mut RelationRetirement,
    ) {
        retirement.unlink_locked(self, &self.peers, |peer| &peer.peers);
        retirement.unlink_locked(self, &self.masters, |master| &master.slaves);
        retirement.unlink_locked(self, &self.slaves, |slave| &slave.masters);
    }

    fn attach_peer_locked(left: &Arc<Self>, right: &Arc<Self>) {
        if Arc::ptr_eq(left, right) || left.is_retired() || right.is_retired() {
            return;
        }
        relations::add_relation(&left.peers, right);
        relations::add_relation(&right.peers, left);
        debug_assert!(relations::has_relation(&left.peers, right));
        debug_assert!(relations::has_relation(&right.peers, left));
    }

    pub(super) fn attach_master_locked(slave: &Arc<Self>, master: &Arc<Self>) {
        if slave.is_retired() || master.is_retired() {
            return;
        }
        relations::add_relation(&slave.masters, master);
        relations::add_relation(&master.slaves, slave);
        debug_assert!(relations::has_relation(&slave.masters, master));
        debug_assert!(relations::has_relation(&master.slaves, slave));
    }

    pub(super) fn set_shared_locked(self: &Arc<Self>, retirement: &mut RelationRetirement) {
        self.leave_propagation_relations_locked(retirement);
        *self.propagation.lock() = PropagationType::Shared;
        if self.peer_group_id.load(Ordering::Acquire) == 0 {
            self.peer_group_id.store(
                PEER_GROUP_COUNTER.fetch_add(1, Ordering::Relaxed),
                Ordering::Release,
            );
        }
    }

    fn set_private_locked(self: &Arc<Self>, retirement: &mut RelationRetirement) {
        self.leave_propagation_relations_locked(retirement);
        *self.propagation.lock() = PropagationType::Private;
        self.peer_group_id.store(0, Ordering::Release);
    }

    fn set_slave_locked(self: &Arc<Self>, retirement: &mut RelationRetirement) {
        let masters = if self.is_shared() {
            retirement.collect_locked(&self.peers)
        } else {
            0..0
        };

        self.leave_propagation_relations_locked(retirement);
        *self.propagation.lock() = PropagationType::Slave;
        self.peer_group_id.store(0, Ordering::Release);
        for index in masters {
            Self::attach_master_locked(self, retirement.owner(index));
        }
    }

    fn set_unbindable_locked(self: &Arc<Self>, retirement: &mut RelationRetirement) {
        self.leave_propagation_relations_locked(retirement);
        *self.propagation.lock() = PropagationType::Unbindable;
        self.peer_group_id.store(0, Ordering::Release);
    }

    fn set_propagation_locked(
        self: &Arc<Self>,
        propagation: PropagationType,
        retirement: &mut RelationRetirement,
    ) {
        match propagation {
            PropagationType::Private => self.set_private_locked(retirement),
            PropagationType::Shared => self.set_shared_locked(retirement),
            PropagationType::Slave => self.set_slave_locked(retirement),
            PropagationType::Unbindable => self.set_unbindable_locked(retirement),
        }
    }

    fn set_propagation_recursive_locked(
        self: &Arc<Self>,
        propagation: PropagationType,
        retirement: &mut RelationRetirement,
    ) {
        let mut frontier = vec![self.clone()];
        while let Some(mountpoint) = frontier.pop() {
            mountpoint.set_propagation_locked(propagation, retirement);
            frontier.extend(mountpoint.children());
        }
    }

    fn set_propagation(self: &Arc<Self>, propagation: PropagationType, recursive: bool) {
        let mut retirement = RelationRetirement::default();
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        if self.is_retired() {
            return;
        }
        if recursive {
            self.set_propagation_recursive_locked(propagation, &mut retirement);
        } else {
            self.set_propagation_locked(propagation, &mut retirement);
        }
        MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
    }

    pub fn set_shared(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Shared, false);
    }

    /// Makes this mount and every descendant mount shared.
    pub fn set_shared_recursive(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Shared, true);
    }

    pub fn set_private(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Private, false);
    }

    /// Makes this mount and every descendant mount private.
    pub fn set_private_recursive(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Private, true);
    }

    pub fn set_slave(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Slave, false);
    }

    /// Makes this mount and every descendant mount a slave mount.
    pub fn set_slave_recursive(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Slave, true);
    }

    pub fn set_unbindable(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Unbindable, false);
    }

    /// Makes this mount and every descendant mount unbindable.
    pub fn set_unbindable_recursive(self: &Arc<Self>) {
        self.set_propagation(PropagationType::Unbindable, true);
    }

    pub(super) fn join_shared_group_locked(
        self: &Arc<Self>,
        source: &Arc<Self>,
        retirement: &mut RelationRetirement,
    ) {
        if self.is_retired() || source.is_retired() {
            return;
        }
        let group = retirement.collect_locked(&source.peers);
        #[cfg(test)]
        clone_relation_hook::after_upgrade(
            source,
            clone_relation_hook::UpgradeKind::SharedPeers,
            retirement.slice(group.clone()),
        );

        self.set_shared_locked(retirement);
        let source_group = source.peer_group_id.load(Ordering::Acquire);
        if source_group != 0 {
            self.peer_group_id.store(source_group, Ordering::Release);
        }
        Self::attach_peer_locked(self, source);
        for index in group {
            Self::attach_peer_locked(self, retirement.owner(index));
        }
    }

    pub fn join_shared_group(self: &Arc<Self>, source: &Arc<Self>) {
        let mut retirement = RelationRetirement::default();
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        self.join_shared_group_locked(source, &mut retirement);
        MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
    }

    fn attach_child(
        location: &Location,
        child: &Arc<Self>,
        retirement: &mut PropagationRetirement,
    ) -> VfsResult<()> {
        location.mountpoint.check_topology_live_locked()?;
        child.check_topology_live_locked()?;
        location.check_is_dir()?;
        let covered = location
            .mountpoint
            .children
            .lock()
            .get(&location.entry.key())
            .cloned();
        let cover_target = if let Some(covered) = &covered {
            retirement.mounts.push(covered.clone());
            covered.check_topology_live_locked()?;
            if covered.root_role() == MountRootRole::NamespaceAnchor {
                return Err(VfsError::InvalidInput);
            }
            let previous = covered.location().ok_or(VfsError::InvalidInput)?;
            let previous = retirement.locations.push(previous);
            if !retirement.locations.get(previous).ptr_eq(location) {
                return Err(VfsError::InvalidInput);
            }
            let topmost = retirement.topmost_root_locked(child)?;
            retirement.locations.get(topmost).check_is_dir()?;
            Some(retirement.locations.get(topmost).clone())
        } else {
            None
        };

        // Keep the exact resolved target below the old cover. If the new
        // tree has its own root covers, preserve those below the old cover too.
        let target_slot = retirement.prepare_child_slot(location, child);
        let cover_slot = covered
            .as_ref()
            .zip(cover_target.as_ref())
            .map(|(covered, target)| {
                let prepared = retirement.prepare_child_slot(target, covered);
                (covered, target, prepared)
            });
        if cover_slot.is_some() {
            retirement.locations.locations.reserve(1);
        }
        retirement
            .mounts
            .reserve(1 + usize::from(cover_slot.is_some()));

        // Everything fallible and every capacity preparation precedes this
        // first connection change. Topology exclusion covers all three writes.
        retirement.publish_child_slot_locked(location, target_slot);
        if let Some((covered, target, prepared)) = cover_slot {
            let previous = covered.location.lock().replace(target.clone());
            if let Some(previous) = previous {
                retirement.locations.push(previous);
            }
            retirement.publish_child_slot_locked(target, prepared);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn propagate_new_child(
        source_parent: &Arc<Self>,
        source_location: &Location,
        child: &Arc<Self>,
    ) -> VfsResult<()> {
        let mut retirement = PropagationRetirement::default();
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        Self::propagate_new_child_locked(source_parent, source_location, child, &mut retirement)
    }

    pub(super) fn propagate_new_child_locked(
        source_parent: &Arc<Self>,
        source_location: &Location,
        child: &Arc<Self>,
        retirement: &mut PropagationRetirement,
    ) -> VfsResult<()> {
        source_parent.check_topology_live_locked()?;
        source_location.mountpoint.check_topology_live_locked()?;
        child.check_topology_live_locked()?;
        let path_components = Self::relative_path_from_mount_root(
            source_location,
            Some(&mut retirement.source_path),
        )?;

        // Walk the entire downstream propagation graph so the child reaches
        // slaves-of-peers and slaves-of-slaves too. Linux requires transitive
        // forwarding along slave chains (A shared -> B slave -> C slave must
        // deliver to C). Shared peer groups are cliques, so a `mount_id`-keyed
        // visited set is mandatory to stop peers from echoing the event back.
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(source_parent.mount_id());
        retirement.collect_receivers_locked(source_parent);
        let mut index = 0;
        while index < retirement.receivers.len() {
            let target_parent = retirement.receivers[index].clone();
            index += 1;
            if target_parent.is_retired() || !visited.insert(target_parent.mount_id()) {
                continue;
            }

            retirement.collect_receivers_locked(&target_parent);

            let location = retirement.resolve_locked(&target_parent, &path_components)?;
            if location.mountpoint.is_retired() {
                continue;
            }
            let inserted_key = location.entry.key();
            let propagated = Self::clone_shallow(child, Some(location.clone()));
            retirement.mounts.push(propagated.clone());
            if target_parent.is_slave() {
                if !child.is_shared() {
                    child.set_shared_locked(&mut retirement.relations);
                }
                propagated.join_shared_group_locked(child, &mut retirement.relations);
                propagated.leave_propagation_relations_locked(&mut retirement.relations);
                *propagated.propagation.lock() = PropagationType::Slave;
                propagated.peer_group_id.store(0, Ordering::Release);
                Self::attach_master_locked(&propagated, child);
            } else {
                if !child.is_shared() {
                    child.set_shared_locked(&mut retirement.relations);
                }
                propagated.join_shared_group_locked(child, &mut retirement.relations);
            }
            Self::attach_child(&location, &propagated, retirement)?;
            let resolved = retirement.resolve_locked(&target_parent, &path_components)?;
            if !Arc::ptr_eq(resolved.mountpoint(), &propagated) {
                warn!(
                    "mount propagation mismatch path={:?} inserted_key={:?} resolved_key={:?} \
                     resolved_is_root={} resolved_mp_device={} replicated_device={}",
                    path_components,
                    inserted_key,
                    resolved.entry.key(),
                    resolved.is_root_of_mount(),
                    resolved.mountpoint().device(),
                    child.device(),
                );
            }
        }
        Ok(())
    }

    pub(super) fn propagation_targets(self: &Arc<Self>) -> Vec<Arc<Self>> {
        let mut targets: Vec<_> = self.peers.lock().iter().filter_map(Weak::upgrade).collect();
        targets.extend(self.slaves.lock().iter().filter_map(Weak::upgrade));
        targets
    }
}

#[cfg(test)]
pub(super) mod clone_relation_hook {
    use core::marker::PhantomData;

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::mount) enum UpgradeKind {
        SharedPeers,
        SlaveMasters,
    }

    struct ReleasePoint {
        source: Weak<Mountpoint>,
        kind: UpgradeKind,
        counterpart: Option<Arc<Mountpoint>>,
        hits: Arc<core::sync::atomic::AtomicUsize>,
    }

    std::thread_local! {
        static RELEASE_POINT: core::cell::RefCell<Option<ReleasePoint>> =
            const { core::cell::RefCell::new(None) };
    }

    /// Registration belongs to this host-test thread and one exact source.
    pub(in crate::mount) struct ArmedRelease {
        _thread_bound: PhantomData<std::rc::Rc<()>>,
    }

    impl Drop for ArmedRelease {
        fn drop(&mut self) {
            let remaining = RELEASE_POINT.with(|slot| slot.borrow_mut().take());
            drop(remaining);
        }
    }

    pub(in crate::mount) fn arm(
        source: &Arc<Mountpoint>,
        kind: UpgradeKind,
        counterpart: Arc<Mountpoint>,
        hits: Arc<core::sync::atomic::AtomicUsize>,
    ) -> ArmedRelease {
        RELEASE_POINT.with(|slot| {
            let mut slot = slot.borrow_mut();
            assert!(slot.is_none(), "clone release point is already armed");
            *slot = Some(ReleasePoint {
                source: Arc::downgrade(source),
                kind,
                counterpart: Some(counterpart),
                hits,
            });
        });
        ArmedRelease {
            _thread_bound: PhantomData,
        }
    }

    pub(in crate::mount) fn after_upgrade(
        source: &Arc<Mountpoint>,
        kind: UpgradeKind,
        upgraded: &[Arc<Mountpoint>],
    ) {
        let external = RELEASE_POINT.with(|slot| {
            let mut slot = slot.borrow_mut();
            let point = slot.as_mut()?;
            if point.source.as_ptr() != Arc::as_ptr(source) || point.kind != kind {
                return None;
            }
            let counterpart = point.counterpart.as_ref()?;
            if !upgraded.iter().any(|mount| Arc::ptr_eq(mount, counterpart)) {
                return None;
            }
            // The real algorithm now owns the upgraded Arc. Move only the
            // external owner out, and end the TLS borrow before releasing it.
            point.hits.fetch_add(1, Ordering::Relaxed);
            point.counterpart.take()
        });
        drop(external);
    }
}
