use hashbrown::HashSet;
use log::warn;

use super::*;

mod relations;
pub(super) use relations::RelationRetirement;

/// Callers declare this before taking topology exclusion. Even receivers
/// rejected as retired retain their successful Weak upgrades until gate exit.
#[derive(Default)]
pub(super) struct PropagationRetirement {
    locations: Vec<Location>,
    receivers: Vec<Arc<Mountpoint>>,
    mounts: Vec<Arc<Mountpoint>>,
    relations: RelationRetirement,
}

impl PropagationRetirement {
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
        self.locations.push(target.root_location());
        for component in components {
            let next = self
                .locations
                .last()
                .expect("propagation root is retained")
                .lookup_no_follow(component)?;
            self.locations.push(next);
        }
        Ok(self
            .locations
            .last()
            .expect("propagation location is retained")
            .clone())
    }
}

impl Mountpoint {
    fn relative_path_from_mount_root(location: &Location) -> VfsResult<Vec<String>> {
        let mut components = Vec::new();
        let mut current = location.clone();
        while !current.is_root_of_mount() {
            let snapshot = current.entry.name_snapshot();
            if snapshot.state() == crate::NameState::Untrusted {
                return Err(VfsError::WouldBlock);
            }
            components.push(snapshot.name().to_owned());
            current = current.wrap(snapshot.parent().cloned().ok_or(VfsError::InvalidInput)?);
        }
        components.reverse();
        Ok(components)
    }

    pub(super) fn corresponding_child(
        parent: &Arc<Self>,
        source_location: &Location,
    ) -> VfsResult<Option<Arc<Self>>> {
        let source_path = Self::relative_path_from_mount_root(source_location)?;
        for child in parent.children() {
            let Some(location) = child.location() else {
                continue;
            };
            if Self::relative_path_from_mount_root(&location)? == source_path {
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
        // Path resolution may have crossed a covering mount. Publish at that
        // exact location, not the receiver at which resolution began.
        let displaced = location
            .mountpoint
            .children
            .lock()
            .insert(location.entry.key(), child.clone());
        if let Some(displaced) = displaced {
            retirement.mounts.push(displaced);
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
        let path_components = Self::relative_path_from_mount_root(source_location)?;

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
