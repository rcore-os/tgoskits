use alloc::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

use ax_sync::RawSpinLockIrqSaveGuard;

use crate::{
    CgroupError, CgroupFileKind, CgroupFsInit, CgroupFsTarget, CgroupResult, ProcessId,
    fs::CgroupFsState, pids::PidsState, sync::RawSpinLock,
};

mod attributes;
mod path;

pub(crate) use path::relative_path;

static NEXT_CGROUP_ID: AtomicU64 = AtomicU64::new(1);
const PIDS_CONTROLLER: &[&str] = &["pids"];
const NO_CONTROLLERS: &[&str] = &[];

fn allocate_id() -> CgroupResult<u64> {
    // This counter only allocates unique values. The hierarchy gate publishes
    // the actual node and all of its initialized inode slots.
    NEXT_CGROUP_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
            (id <= CgroupFsTarget::MAX_NODE_ID).then_some(id + 1)
        })
        .map_err(|_| CgroupError::NoSpace)
}

/// The non-sleeping mutation boundary shared by one hierarchy.
///
/// Lock order is process membership, membership ledger, this gate, then node
/// locks. Never acquire the ledger or call OS/filesystem code under this gate.
pub struct CgroupMutationDomain {
    gate: RawSpinLock<()>,
}

/// An actual IRQ-saving hierarchy lock, never a cross-scheduling reservation.
///
/// Removed owners and acquired pins must outlive this guard: dropping their
/// final owner or a pin while holding the gate can re-enter cleanup.
pub struct CgroupMutationGuard<'a> {
    domain: &'a CgroupMutationDomain,
    _gate: RawSpinLockIrqSaveGuard<'a, ()>,
}

struct NodeLifetime {
    live: bool,
    pins: usize,
    reservations: usize,
}

/// A stable node in the cgroup v2 hierarchy.
pub struct CgroupNode {
    id: u64,
    name: String,
    parent: Option<Weak<Self>>,
    children: RawSpinLock<BTreeMap<String, Arc<Self>>>,
    members: RawSpinLock<BTreeSet<ProcessId>>,
    pids: PidsState,
    pids_enabled_for_children: Arc<AtomicBool>,
    parent_pids_enabled: Option<Arc<AtomicBool>>,
    domain: Arc<CgroupMutationDomain>,
    lifetime: RawSpinLock<NodeLifetime>,
    fs: RawSpinLock<CgroupFsState>,
}

/// An ownership reference that prevents removal of a cgroup hierarchy node.
pub struct CgroupPin {
    node: Arc<CgroupNode>,
}

/// A non-copyable fork right that prevents removal until rollback or commit.
pub(crate) struct CgroupReservation {
    node: Arc<CgroupNode>,
}

impl CgroupMutationDomain {
    /// Lock this hierarchy without sleeping, saving local interrupt state.
    pub fn lock(&self) -> CgroupMutationGuard<'_> {
        CgroupMutationGuard {
            domain: self,
            _gate: self.gate.lock_irqsave(),
        }
    }
}

impl CgroupNode {
    pub(crate) fn new_root(init: CgroupFsInit) -> CgroupResult<Arc<Self>> {
        init.validate()?;
        let id = allocate_id()?;
        Ok(Arc::new(Self {
            id,
            name: String::new(),
            parent: None,
            children: RawSpinLock::new(BTreeMap::new()),
            members: RawSpinLock::new(BTreeSet::new()),
            pids: PidsState::new(),
            pids_enabled_for_children: Arc::new(AtomicBool::new(false)),
            parent_pids_enabled: None,
            domain: Arc::new(CgroupMutationDomain {
                gate: RawSpinLock::new(()),
            }),
            lifetime: RawSpinLock::new(NodeLifetime {
                live: true,
                pins: 0,
                reservations: 0,
            }),
            fs: RawSpinLock::new(CgroupFsState::new(id, init, false)),
        }))
    }

    /// Return the stable internal node ID.
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Return the local directory name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Upgrade and return the parent node.
    pub fn parent(&self) -> Option<Arc<Self>> {
        self.parent.as_ref().and_then(Weak::upgrade)
    }

    pub(crate) fn has_parent(&self) -> bool {
        self.parent.is_some()
    }

    /// Return the hierarchy owner for an already-locked domain attempt.
    pub fn mutation_domain(&self) -> &CgroupMutationDomain {
        &self.domain
    }

    pub(crate) fn shares_domain(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.domain, &other.domain)
    }

    /// Return controllers this cgroup may enable for its direct children.
    pub fn available_controllers(&self) -> &'static [&'static str] {
        // Borrow the parent's controller state, not a temporary parent-node Arc:
        // a weak upgrade under a mutation guard could become its final owner.
        if self
            .parent_pids_enabled
            .as_ref()
            .is_none_or(|enabled| enabled.load(Ordering::Acquire))
        {
            PIDS_CONTROLLER
        } else {
            NO_CONTROLLERS
        }
    }

    /// Return controllers enabled for direct child cgroups.
    pub fn enabled_subtree_controllers(&self) -> &'static [&'static str] {
        if self.pids_enabled_for_children.load(Ordering::Acquire) {
            PIDS_CONTROLLER
        } else {
            NO_CONTROLLERS
        }
    }

    /// Return whether this non-root cgroup exposes pids interface files.
    pub fn has_pids_interface(&self) -> bool {
        self.parent.is_some() && self.available_controllers().contains(&"pids")
    }

    /// Render `pids.max`, including for a retained, removed node.
    pub fn pids_max_text(&self) -> CgroupResult<String> {
        self.require_pids_interface()?;
        Ok(self.pids.maximum_text())
    }

    /// Render `pids.current`.
    pub fn pids_current_text(&self) -> CgroupResult<String> {
        self.require_pids_interface()?;
        Ok(self.pids.current_text())
    }

    /// Render the lifetime high-water mark from `pids.peak`.
    pub fn pids_peak_text(&self) -> CgroupResult<String> {
        self.require_pids_interface()?;
        Ok(self.pids.peak_text())
    }

    /// Render `pids.events`.
    pub fn pids_events_text(&self) -> CgroupResult<String> {
        self.require_pids_interface()?;
        Ok(self.pids.events_text())
    }

    /// Update `pids.max`.
    pub fn write_pids_max(&self, data: &str) -> CgroupResult<()> {
        self.domain.lock().write_pids_max(self, data)
    }

    /// Create a direct child, rejecting a retained dead parent.
    pub fn create_child(
        self: &Arc<Self>,
        name: &str,
        init: CgroupFsInit,
    ) -> CgroupResult<Arc<Self>> {
        self.domain.lock().create_child(self, name, init)
    }

    /// Look up a direct child.
    pub fn lookup_child(&self, name: &str) -> CgroupResult<Arc<Self>> {
        self.domain.lock().lookup_child(self, name)
    }

    /// List direct child names.
    pub fn child_names(&self) -> Vec<String> {
        self.children.lock_irqsave().keys().cloned().collect()
    }

    /// Whether this node contains any child groups, excluding interface files.
    pub fn has_children(&self) -> bool {
        !self.children.lock_irqsave().is_empty()
    }

    /// Remove an empty child; ordinary retained Arcs do not prevent removal.
    pub fn remove_child(&self, name: &str, now: Duration) -> CgroupResult<()> {
        let retired = {
            let guard = self.domain.lock();
            guard.remove_child(self, name, now)?
        };
        drop(retired);
        Ok(())
    }

    /// Return a sorted snapshot of member process IDs.
    pub fn members(&self) -> Vec<ProcessId> {
        self.members.lock_irqsave().iter().copied().collect()
    }

    pub(crate) fn has_member(&self, pid: ProcessId) -> bool {
        self.members.lock_irqsave().contains(&pid)
    }

    /// Obtain a first namespace or mount pin only while this node is live.
    pub fn try_pin(self: &Arc<Self>) -> CgroupResult<CgroupPin> {
        self.domain.lock().try_pin(self)
    }

    fn require_pids_interface(&self) -> CgroupResult<()> {
        self.has_pids_interface()
            .then_some(())
            .ok_or(CgroupError::NotFound)
    }
}

impl CgroupMutationGuard<'_> {
    /// Reject a node from another hierarchy without taking its gate.
    pub fn check_domain(&self, node: &CgroupNode) -> CgroupResult<()> {
        if core::ptr::eq(self.domain, node.domain.as_ref()) {
            Ok(())
        } else {
            Err(CgroupError::InvalidInput)
        }
    }

    /// Check business liveness in the same attempt as the following mutation.
    pub fn check_live(&self, node: &CgroupNode) -> CgroupResult<()> {
        self.check_domain(node)?;
        if node.lifetime.lock_irqsave().live {
            Ok(())
        } else {
            Err(CgroupError::Removed)
        }
    }

    /// Update the existing pids limit without re-acquiring the hierarchy gate.
    pub fn write_pids_max(&self, node: &CgroupNode, data: &str) -> CgroupResult<()> {
        self.check_live(node)?;
        node.require_pids_interface()?;
        node.pids.set_maximum(data)
    }

    /// Create a child in this attempt.
    ///
    /// Like the existing membership B-trees, insertion requires a non-sleeping
    /// allocator with no callbacks into cgroups. The kernel heap and its bounded,
    /// try-lock-only pressure reclaim satisfy that requirement.
    pub fn create_child(
        &self,
        parent: &Arc<CgroupNode>,
        name: &str,
        init: CgroupFsInit,
    ) -> CgroupResult<Arc<CgroupNode>> {
        self.check_live(parent)?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(CgroupError::InvalidInput);
        }
        if CgroupFileKind::from_name(name).is_some() {
            return Err(CgroupError::AlreadyExists);
        }
        init.validate()?;

        let mut children = parent.children.lock_irqsave();
        let entry = match children.entry(name.to_string()) {
            Entry::Occupied(_) => return Err(CgroupError::AlreadyExists),
            Entry::Vacant(entry) => entry,
        };
        let id = allocate_id()?;
        let child = Arc::new(CgroupNode {
            id,
            name: name.to_string(),
            parent: Some(Arc::downgrade(parent)),
            children: RawSpinLock::new(BTreeMap::new()),
            members: RawSpinLock::new(BTreeSet::new()),
            pids: PidsState::new(),
            pids_enabled_for_children: Arc::new(AtomicBool::new(false)),
            parent_pids_enabled: Some(Arc::clone(&parent.pids_enabled_for_children)),
            domain: Arc::clone(&parent.domain),
            lifetime: RawSpinLock::new(NodeLifetime {
                live: true,
                pins: 0,
                reservations: 0,
            }),
            fs: RawSpinLock::new(CgroupFsState::new(
                id,
                init,
                parent.pids_enabled_for_children.load(Ordering::Relaxed),
            )),
        });
        // The checked vacant entry cannot retire an old node under the gate.
        entry.insert(Arc::clone(&child));
        drop(children);
        parent.fs.lock_irqsave().touch_directory(init.now);
        Ok(child)
    }

    /// Look up a direct child.
    pub fn lookup_child(&self, parent: &CgroupNode, name: &str) -> CgroupResult<Arc<CgroupNode>> {
        self.check_domain(parent)?;
        parent
            .children
            .lock_irqsave()
            .get(name)
            .cloned()
            .ok_or(CgroupError::NotFound)
    }

    /// Mark an empty child dead and detach it atomically.
    ///
    /// The returned owner must be retired after this guard and any ledger lock.
    pub fn remove_child(
        &self,
        parent: &CgroupNode,
        name: &str,
        now: Duration,
    ) -> CgroupResult<Arc<CgroupNode>> {
        self.check_live(parent)?;
        // The gate stabilizes the parent table, so parent/child local locks
        // need not be nested in the same lock class.
        let child = self.lookup_child(parent, name)?;
        self.check_live(&child)?;
        if !child.children.lock_irqsave().is_empty() {
            return Err(CgroupError::DirectoryNotEmpty);
        }
        let mut lifetime = child.lifetime.lock_irqsave();
        if !child.members.lock_irqsave().is_empty()
            || lifetime.pins != 0
            || lifetime.reservations != 0
        {
            return Err(CgroupError::ResourceBusy);
        }
        lifetime.live = false;
        drop(lifetime);
        // Return the table's owner; `child` cannot be the last owner here.
        let retired = parent
            .children
            .lock_irqsave()
            .remove(name)
            .expect("child slot is stable under its hierarchy gate");
        parent.fs.lock_irqsave().touch_directory(now);
        Ok(retired)
    }

    pub(crate) fn add_member(&self, node: &CgroupNode, pid: ProcessId) -> CgroupResult<()> {
        self.check_live(node)?;
        if node.members.lock_irqsave().insert(pid) {
            Ok(())
        } else {
            Err(CgroupError::ResourceBusy)
        }
    }

    pub(crate) fn remove_member(&self, node: &CgroupNode, pid: ProcessId) -> CgroupResult<()> {
        self.check_domain(node)?;
        if node.members.lock_irqsave().remove(&pid) {
            Ok(())
        } else {
            Err(CgroupError::NoSuchProcess)
        }
    }

    pub(crate) fn try_charge_pids(&self, node: &CgroupNode) -> CgroupResult<()> {
        self.check_live(node)?;
        node.pids.try_charge()
    }

    pub(crate) fn charge_pids_unchecked(&self, node: &CgroupNode, count: u64) {
        self.check_domain(node)
            .expect("charged path must share its hierarchy");
        node.pids.charge_unchecked(count);
    }

    pub(crate) fn uncharge_pids(&self, node: &CgroupNode, count: u64) {
        self.check_domain(node)
            .expect("charged path must share its hierarchy");
        node.pids.uncharge(count);
    }

    pub(crate) fn record_pids_max_event(&self, node: &CgroupNode) {
        self.check_domain(node)
            .expect("charged path must share its hierarchy");
        node.pids.record_max_event();
    }

    /// Obtain a first pin without re-acquiring this attempt's gate.
    pub fn try_pin(&self, node: &Arc<CgroupNode>) -> CgroupResult<CgroupPin> {
        self.check_live(node)?;
        let mut lifetime = node.lifetime.lock_irqsave();
        lifetime.pins = lifetime
            .pins
            .checked_add(1)
            .ok_or(CgroupError::ResourceBusy)?;
        Ok(CgroupPin {
            node: Arc::clone(node),
        })
    }

    pub(crate) fn reserve(&self, node: &Arc<CgroupNode>) -> CgroupResult<CgroupReservation> {
        self.check_live(node)?;
        let mut lifetime = node.lifetime.lock_irqsave();
        lifetime.reservations = lifetime
            .reservations
            .checked_add(1)
            .ok_or(CgroupError::ResourceBusy)?;
        Ok(CgroupReservation {
            node: Arc::clone(node),
        })
    }
}

impl CgroupPin {
    /// Derive an independent logical pin from this already-live ownership right.
    pub fn duplicate(&self) -> Self {
        let _guard = self.node.domain.lock();
        let mut lifetime = self.node.lifetime.lock_irqsave();
        assert!(
            lifetime.live && lifetime.pins > 0,
            "an existing pin keeps its node live"
        );
        lifetime.pins = lifetime
            .pins
            .checked_add(1)
            .expect("cgroup pin count exhausted");
        Self {
            node: Arc::clone(&self.node),
        }
    }

    /// Clone the pinned node handle without creating another logical pin.
    pub fn node(&self) -> Arc<CgroupNode> {
        Arc::clone(&self.node)
    }
}

impl Drop for CgroupPin {
    fn drop(&mut self) {
        let _guard = self.node.domain.lock();
        let mut lifetime = self.node.lifetime.lock_irqsave();
        lifetime.pins = lifetime
            .pins
            .checked_sub(1)
            .expect("cgroup pin count underflow");
    }
}

impl Drop for CgroupReservation {
    fn drop(&mut self) {
        let _guard = self.node.domain.lock();
        let mut lifetime = self.node.lifetime.lock_irqsave();
        lifetime.reservations = lifetime
            .reservations
            .checked_sub(1)
            .expect("cgroup reservation count underflow");
        // Both locks leave scope before the node field's automatic drop.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CgroupFsAttributes, CgroupFsUpdate, TEST_FS_INIT};

    #[test]
    fn renders_paths_relative_to_namespace_root() {
        let root = CgroupNode::new_root(TEST_FS_INIT).unwrap();
        let parent = root.create_child("parent", TEST_FS_INIT).unwrap();
        let child = parent.create_child("child", TEST_FS_INIT).unwrap();
        let sibling = root.create_child("sibling", TEST_FS_INIT).unwrap();

        assert_eq!(relative_path(&parent, &parent), "/");
        assert_eq!(relative_path(&parent, &child), "/child");
        assert_eq!(relative_path(&child, &parent), "/..");
        assert_eq!(relative_path(&child, &sibling), "/../../sibling");
    }

    #[test]
    fn rejects_removal_while_node_is_pinned() {
        let root = CgroupNode::new_root(TEST_FS_INIT).unwrap();
        let child = root.create_child("child", TEST_FS_INIT).unwrap();
        let incidental_reference = child.clone();
        let pin = child.try_pin().unwrap();
        let namespace = crate::CgroupNamespace::new(Arc::clone(&child)).unwrap();
        let mount_pin = namespace.pin_root();
        drop(namespace);
        drop(child);

        assert_eq!(
            root.remove_child("child", TEST_FS_INIT.now),
            Err(CgroupError::ResourceBusy)
        );

        drop(mount_pin);
        drop(pin);
        assert_eq!(root.remove_child("child", TEST_FS_INIT.now), Ok(()));
        assert_eq!(incidental_reference.name(), "child");
        let late_create = incidental_reference
            .create_child("late", TEST_FS_INIT)
            .map(|_| ());
        let still_absent = root.lookup_child("child").map(|_| ());
        assert_eq!(
            (late_create, still_absent),
            (Err(CgroupError::Removed), Err(CgroupError::NotFound))
        );
        assert!(matches!(
            incidental_reference.try_pin(),
            Err(CgroupError::Removed)
        ));
        assert!(matches!(
            crate::CgroupNamespace::new(Arc::clone(&incidental_reference)),
            Err(CgroupError::Removed)
        ));
        // Ordinary references preserve attributes, not permission to relink.
        let before = incidental_reference
            .fs_attributes(CgroupFsTarget::Directory)
            .unwrap();
        incidental_reference
            .update_fs_attributes(
                CgroupFsTarget::Directory,
                CgroupFsUpdate {
                    uid: Some(42),
                    ..Default::default()
                },
            )
            .unwrap();
        let after = incidental_reference
            .fs_attributes(CgroupFsTarget::Directory)
            .unwrap();
        assert_eq!(after, CgroupFsAttributes { uid: 42, ..before });
    }

    #[test]
    fn exposes_pids_only_after_the_parent_enables_it() {
        let root = CgroupNode::new_root(TEST_FS_INIT).unwrap();
        let creator = CgroupFsInit {
            uid: 1001,
            gid: 1002,
            mode: 0o1700,
            now: Duration::from_secs(5),
        };
        let child = root.create_child("child", creator).unwrap();
        let alias = Arc::clone(&root.lookup_child("child").unwrap());
        let directory = child.fs_attributes(CgroupFsTarget::Directory).unwrap();
        let procs_target = CgroupFsTarget::File(CgroupFileKind::Procs);
        let procs = child.fs_attributes(procs_target).unwrap();
        assert_eq!(
            (
                directory.uid,
                directory.gid,
                directory.mode,
                directory.ctime
            ),
            (creator.uid, creator.gid, creator.mode, creator.now)
        );
        assert_eq!(
            (procs.uid, procs.gid, procs.mode, procs.ctime),
            (
                creator.uid,
                creator.gid,
                CgroupFileKind::Procs.mode(),
                creator.now
            )
        );
        alias
            .update_fs_attributes(
                CgroupFsTarget::Directory,
                CgroupFsUpdate {
                    mode: Some(0o711),
                    gid: Some(2002),
                    ctime: Some(Duration::from_secs(7)),
                    ..Default::default()
                },
            )
            .unwrap();
        let changed = child.fs_attributes(CgroupFsTarget::Directory).unwrap();
        assert_eq!(
            changed,
            CgroupFsAttributes {
                gid: 2002,
                mode: 0o711,
                ctime: Duration::from_secs(7),
                ..directory
            }
        );
        child
            .update_fs_attributes(
                procs_target,
                CgroupFsUpdate {
                    uid: Some(3001),
                    atime: Some(Duration::from_secs(9)),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            alias.fs_attributes(procs_target).unwrap(),
            CgroupFsAttributes {
                uid: 3001,
                atime: Duration::from_secs(9),
                ..procs
            }
        );
        assert_eq!(
            alias.fs_attributes(CgroupFsTarget::Directory).unwrap(),
            changed
        );
        assert_eq!(
            child.update_fs_attributes(
                procs_target,
                CgroupFsUpdate {
                    mode: Some(0o10000),
                    uid: Some(9001),
                    ..Default::default()
                },
            ),
            Err(CgroupError::InvalidInput)
        );
        assert_eq!(child.fs_attributes(procs_target).unwrap().uid, 3001);

        assert_eq!(root.available_controllers(), ["pids"]);
        assert!(!child.has_pids_interface());
        assert_eq!(child.pids_max_text(), Err(CgroupError::NotFound));
        assert_eq!(child.pids_peak_text(), Err(CgroupError::NotFound));

        let enabler = CgroupFsInit {
            uid: 4001,
            gid: 4002,
            now: Duration::from_secs(11),
            ..TEST_FS_INIT
        };
        root.write_subtree_control("+pids", enabler).unwrap();

        assert!(child.has_pids_interface());
        assert_eq!(child.pids_max_text(), Ok(String::from("max\n")));
        assert_eq!(child.pids_peak_text(), Ok(String::from("0\n")));
        for kind in [
            CgroupFileKind::PidsMax,
            CgroupFileKind::PidsCurrent,
            CgroupFileKind::PidsPeak,
            CgroupFileKind::PidsEvents,
        ] {
            let target = CgroupFsTarget::File(kind);
            let attributes = child.fs_attributes(target).unwrap();
            assert_eq!(
                (attributes.uid, attributes.gid, attributes.atime),
                (enabler.uid, enabler.gid, enabler.now)
            );
            assert_ne!(attributes.inode, directory.inode);
            alias
                .update_fs_attributes(
                    target,
                    CgroupFsUpdate {
                        gid: Some(5002),
                        ..Default::default()
                    },
                )
                .unwrap();
            root.write_subtree_control("+pids", creator).unwrap();
            assert_eq!(
                child.fs_attributes(target).unwrap(),
                CgroupFsAttributes {
                    gid: 5002,
                    ..attributes
                }
            );
        }
        let late = root.create_child("late", creator).unwrap();
        let late_pids = late
            .fs_attributes(CgroupFsTarget::File(CgroupFileKind::PidsMax))
            .unwrap();
        assert_eq!(
            (
                late_pids.uid,
                late_pids.gid,
                late_pids.mode,
                late_pids.ctime
            ),
            (
                creator.uid,
                creator.gid,
                CgroupFileKind::PidsMax.mode(),
                creator.now
            )
        );
    }

    #[test]
    fn removes_empty_child_from_dynamic_parent() {
        let root = CgroupNode::new_root(TEST_FS_INIT).unwrap();
        let parent = root.create_child("parent", TEST_FS_INIT).unwrap();
        let created = Duration::from_secs(13);
        let child = parent
            .create_child(
                "child",
                CgroupFsInit {
                    now: created,
                    ..TEST_FS_INIT
                },
            )
            .unwrap();
        assert!(parent.child_names().contains(&"child".to_string()));
        assert!(child.child_names().is_empty());

        let after_create = parent.fs_attributes(CgroupFsTarget::Directory).unwrap();
        assert_eq!((after_create.mtime, after_create.ctime), (created, created));
        let removed = Duration::from_secs(17);
        assert_eq!(parent.remove_child("child", removed), Ok(()));
        let after_remove = parent.fs_attributes(CgroupFsTarget::Directory).unwrap();
        assert_eq!(
            after_remove,
            CgroupFsAttributes {
                mtime: removed,
                ctime: removed,
                ..after_create
            }
        );
    }
}
