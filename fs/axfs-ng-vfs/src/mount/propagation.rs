use hashbrown::HashSet;
use log::warn;

use super::*;

impl Mountpoint {
    fn relative_path_from_mount_root(location: &Location) -> VfsResult<Vec<String>> {
        let mut components = Vec::new();
        let mut current = location.clone();
        while !current.is_root_of_mount() {
            components.push(current.name().into_owned());
            current = current.parent().ok_or(VfsError::InvalidInput)?;
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

    pub(super) fn rebuild_cloned_relations(clones: &[(Arc<Self>, Arc<Self>)]) {
        for (source, cloned) in clones {
            if source.is_shared() {
                cloned.join_shared_group_locked(source);
            }
        }

        // Joining a shared group clears the clone's previous propagation
        // relations. Rebuild slave edges only after every shared clone has
        // joined its group so traversal order cannot remove an attached slave.
        for (source, cloned) in clones {
            if !source.is_slave() {
                continue;
            }
            let masters: Vec<_> = source
                .masters
                .lock()
                .iter()
                .filter_map(Weak::upgrade)
                .collect();
            for master in masters {
                let cloned_master = clones
                    .iter()
                    .find(|(candidate, _)| Arc::ptr_eq(candidate, &master))
                    .map(|(_, clone)| clone)
                    .unwrap_or(&master);
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

    fn remove_from_shared_group(self: &Arc<Self>) {
        let peers = Self::prune_relations(&mut self.peers.lock());
        for peer in peers {
            peer.peers.lock().retain(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|mountpoint| !Arc::ptr_eq(&mountpoint, self))
            });
        }
        self.peers.lock().clear();
    }

    fn remove_from_masters(self: &Arc<Self>) {
        let masters = Self::prune_relations(&mut self.masters.lock());
        for master in masters {
            master.slaves.lock().retain(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|mountpoint| !Arc::ptr_eq(&mountpoint, self))
            });
        }
        self.masters.lock().clear();
    }

    fn remove_slaves(self: &Arc<Self>) {
        let slaves = Self::prune_relations(&mut self.slaves.lock());
        for slave in slaves {
            slave.masters.lock().retain(|candidate| {
                candidate
                    .upgrade()
                    .is_some_and(|mountpoint| !Arc::ptr_eq(&mountpoint, self))
            });
        }
        self.slaves.lock().clear();
    }

    pub(super) fn leave_propagation_relations_locked(self: &Arc<Self>) {
        self.remove_from_shared_group();
        self.remove_from_masters();
        self.remove_slaves();
    }

    fn prune_relations(relations: &mut Vec<Weak<Self>>) -> Vec<Arc<Self>> {
        let mut live = Vec::new();
        relations.retain(|relation| {
            let Some(mountpoint) = relation.upgrade() else {
                return false;
            };
            if live
                .iter()
                .any(|candidate| Arc::ptr_eq(candidate, &mountpoint))
            {
                return false;
            }
            live.push(mountpoint);
            true
        });
        live
    }

    fn add_relation(relations: &mut Vec<Weak<Self>>, mountpoint: &Arc<Self>) {
        let live = Self::prune_relations(relations);
        if !live
            .iter()
            .any(|candidate| Arc::ptr_eq(candidate, mountpoint))
        {
            relations.push(Arc::downgrade(mountpoint));
        }
    }

    fn has_relation(relations: &Mutex<Vec<Weak<Self>>>, mountpoint: &Arc<Self>) -> bool {
        relations
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .any(|candidate| Arc::ptr_eq(&candidate, mountpoint))
    }

    fn attach_peer_locked(left: &Arc<Self>, right: &Arc<Self>) {
        if Arc::ptr_eq(left, right) {
            return;
        }
        Self::add_relation(&mut left.peers.lock(), right);
        Self::add_relation(&mut right.peers.lock(), left);
        debug_assert!(Self::has_relation(&left.peers, right));
        debug_assert!(Self::has_relation(&right.peers, left));
    }

    pub(super) fn attach_master_locked(slave: &Arc<Self>, master: &Arc<Self>) {
        Self::add_relation(&mut slave.masters.lock(), master);
        Self::add_relation(&mut master.slaves.lock(), slave);
        debug_assert!(Self::has_relation(&slave.masters, master));
        debug_assert!(Self::has_relation(&master.slaves, slave));
    }

    pub(super) fn set_shared_locked(self: &Arc<Self>) {
        self.leave_propagation_relations_locked();
        *self.propagation.lock() = PropagationType::Shared;
        if self.peer_group_id.load(Ordering::Acquire) == 0 {
            self.peer_group_id.store(
                PEER_GROUP_COUNTER.fetch_add(1, Ordering::Relaxed),
                Ordering::Release,
            );
        }
    }

    fn set_private_locked(self: &Arc<Self>) {
        self.leave_propagation_relations_locked();
        *self.propagation.lock() = PropagationType::Private;
        self.peer_group_id.store(0, Ordering::Release);
    }

    fn set_slave_locked(self: &Arc<Self>) {
        let mut masters = Vec::new();
        if self.is_shared() {
            masters.extend(self.peers.lock().iter().filter_map(Weak::upgrade));
        }

        self.leave_propagation_relations_locked();
        *self.propagation.lock() = PropagationType::Slave;
        self.peer_group_id.store(0, Ordering::Release);
        for master in masters {
            Self::attach_master_locked(self, &master);
        }
    }

    fn set_unbindable_locked(self: &Arc<Self>) {
        self.leave_propagation_relations_locked();
        *self.propagation.lock() = PropagationType::Unbindable;
        self.peer_group_id.store(0, Ordering::Release);
    }

    fn set_propagation_locked(self: &Arc<Self>, propagation: PropagationType) {
        match propagation {
            PropagationType::Private => self.set_private_locked(),
            PropagationType::Shared => self.set_shared_locked(),
            PropagationType::Slave => self.set_slave_locked(),
            PropagationType::Unbindable => self.set_unbindable_locked(),
        }
    }

    fn set_propagation_recursive_locked(self: &Arc<Self>, propagation: PropagationType) {
        let mut frontier = vec![self.clone()];
        while let Some(mountpoint) = frontier.pop() {
            mountpoint.set_propagation_locked(propagation);
            frontier.extend(mountpoint.children());
        }
    }

    fn set_propagation(self: &Arc<Self>, propagation: PropagationType, recursive: bool) {
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        if recursive {
            self.set_propagation_recursive_locked(propagation);
        } else {
            self.set_propagation_locked(propagation);
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

    pub(super) fn join_shared_group_locked(self: &Arc<Self>, source: &Arc<Self>) {
        let mut group = vec![source.clone()];
        group.extend(Self::prune_relations(&mut source.peers.lock()));

        self.set_shared_locked();
        let source_group = source.peer_group_id.load(Ordering::Acquire);
        if source_group != 0 {
            self.peer_group_id.store(source_group, Ordering::Release);
        }
        for member in group {
            Self::attach_peer_locked(self, &member);
        }
    }

    pub fn join_shared_group(self: &Arc<Self>, source: &Arc<Self>) {
        let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
        self.join_shared_group_locked(source);
        MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
    }

    fn attach_child(parent: &Arc<Self>, location: Location, child: &Arc<Self>) -> VfsResult<()> {
        location.check_is_dir()?;
        parent
            .children
            .lock()
            .insert(location.entry.key(), child.clone());
        Ok(())
    }

    pub(super) fn propagate_new_child(
        source_parent: &Arc<Self>,
        source_location: &Location,
        child: &Arc<Self>,
    ) -> VfsResult<()> {
        let path_components = Self::relative_path_from_mount_root(source_location)?;

        // Walk the entire downstream propagation graph so the child reaches
        // slaves-of-peers and slaves-of-slaves too. Linux requires transitive
        // forwarding along slave chains (A shared -> B slave -> C slave must
        // deliver to C). Shared peer groups are cliques, so a `mount_id`-keyed
        // visited set is mandatory to stop peers from echoing the event back.
        let mut visited: HashSet<u64> = HashSet::new();
        visited.insert(source_parent.mount_id());
        let mut frontier: Vec<Arc<Self>> = source_parent.propagation_targets();

        while let Some(target_parent) = frontier.pop() {
            if !visited.insert(target_parent.mount_id()) {
                continue;
            }

            for receiver in target_parent.propagation_targets() {
                frontier.push(receiver);
            }

            let mut location = target_parent.root_location();
            for component in &path_components {
                location = location.lookup_no_follow(component)?;
            }
            let inserted_key = location.entry.key();
            let propagated = Self::clone_shallow(child, Some(location.clone()));
            if target_parent.is_slave() {
                if !child.is_shared() {
                    child.set_shared_locked();
                }
                propagated.join_shared_group_locked(child);
                propagated.leave_propagation_relations_locked();
                *propagated.propagation.lock() = PropagationType::Slave;
                propagated.peer_group_id.store(0, Ordering::Release);
                Self::attach_master_locked(&propagated, child);
            } else {
                if !child.is_shared() {
                    child.set_shared_locked();
                }
                propagated.join_shared_group_locked(child);
            }
            Self::attach_child(&target_parent, location, &propagated)?;
            let mut resolved = target_parent.root_location();
            for component in &path_components {
                resolved = resolved.lookup_no_follow(component)?;
            }
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
