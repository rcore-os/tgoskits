//! Final Core ownership of topology, distinct from normal or lazy unmount.
use super::*;

impl Drop for MountNamespace {
    fn drop(&mut self) {
        let retirement = {
            let _topology = MOUNT_TOPOLOGY_MUTATION.lock();
            let mut retirement = NamespaceRetirement::collect_locked(&self.root_mount);
            retirement.disconnect_locked();
            MOUNT_TOPOLOGY_VERSION.fetch_add(1, Ordering::AcqRel);
            retirement
        };
        // This includes every scan Arc, removed connection, attachment guard,
        // and upgraded propagation counterpart. Mount leases remain on nodes.
        drop(retirement);
    }
}

struct NamespaceRetirement {
    nodes: Vec<RetiringMount>,
}

struct RetiringMount {
    parent: Option<Location>,
    children: HashMap<DirEntryId, Arc<Mountpoint>>,
    attachment: Option<Arc<dyn Any + Send + Sync>>,
    peers: RetiringRelations,
    masters: RetiringRelations,
    slaves: RetiringRelations,
    mount: Arc<Mountpoint>,
}

#[derive(Default)]
struct RetiringRelations {
    removed: Vec<Weak<Mountpoint>>,
    counterparts: Vec<Arc<Mountpoint>>,
}

impl RetiringRelations {
    fn collect_locked(relations: &RawSpinLock<Vec<Weak<Mountpoint>>>) -> Self {
        // Topology exclusion keeps the list stable. Reserve before taking its
        // short lock; every successful upgrade moves directly into retirement.
        let capacity = relations.lock().len();
        let mut result = Self {
            removed: Vec::new(),
            counterparts: Vec::with_capacity(capacity),
        };
        for relation in relations.lock().iter() {
            if let Some(mount) = relation.upgrade() {
                result.counterparts.push(mount);
            }
        }
        result
    }

    fn disconnect_locked(
        &mut self,
        mount: &Arc<Mountpoint>,
        relations: &RawSpinLock<Vec<Weak<Mountpoint>>>,
        reverse: impl Fn(&Mountpoint) -> &RawSpinLock<Vec<Weak<Mountpoint>>>,
    ) {
        self.removed = core::mem::take(&mut *relations.lock());
        for counterpart in &self.counterparts {
            // Only remove this exact allocation. Do not upgrade unrelated
            // candidates in retain: their last strong release could tear down
            // an unrelated filesystem while topology or a node lock is held.
            reverse(counterpart)
                .lock()
                .retain(|candidate| candidate.as_ptr() != Arc::as_ptr(mount));
        }
    }
}

impl RetiringMount {
    fn new(mount: Arc<Mountpoint>) -> Self {
        Self {
            parent: None,
            children: HashMap::default(),
            attachment: None,
            peers: RetiringRelations::default(),
            masters: RetiringRelations::default(),
            slaves: RetiringRelations::default(),
            mount,
        }
    }

    fn collect_relations_locked(&mut self) {
        self.peers = RetiringRelations::collect_locked(&self.mount.peers);
        self.masters = RetiringRelations::collect_locked(&self.mount.masters);
        self.slaves = RetiringRelations::collect_locked(&self.mount.slaves);
    }

    fn disconnect_locked(&mut self) {
        self.parent = self.mount.location.lock().take();
        self.children = core::mem::take(&mut *self.mount.children.lock());
        self.attachment = self.mount.lifetime_guard.lock().take();
        self.peers
            .disconnect_locked(&self.mount, &self.mount.peers, |peer| &peer.peers);
        self.masters
            .disconnect_locked(&self.mount, &self.mount.masters, |master| &master.slaves);
        self.slaves
            .disconnect_locked(&self.mount, &self.mount.slaves, |slave| &slave.masters);
        *self.mount.propagation.lock() = PropagationType::Private;
        self.mount.peer_group_id.store(0, Ordering::Release);
    }
}

impl NamespaceRetirement {
    fn collect_locked(root: &Arc<Mountpoint>) -> Self {
        let mut nodes = vec![RetiringMount::new(root.clone())];
        let mut index = 0;
        // Retain the entire reachable tree, not a work stack which drops each
        // scanned node on pop. Nodes moved out before this gate are not owned.
        while index < nodes.len() {
            let mount = nodes[index].mount.clone();
            let count = mount.children.lock().len();
            nodes.reserve(count);
            for child in mount.children.lock().values() {
                nodes.push(RetiringMount::new(child.clone()));
            }
            index += 1;
        }
        for node in &mut nodes {
            node.collect_relations_locked();
        }
        Self { nodes }
    }

    fn disconnect_locked(&mut self) {
        // Close every old publication capability before severing even the first
        // edge. Existing paths and counted uses are deliberately untouched.
        for node in &self.nodes {
            node.mount.retired.store(true, Ordering::Relaxed);
        }
        for node in &mut self.nodes {
            node.disconnect_locked();
        }
    }
}
