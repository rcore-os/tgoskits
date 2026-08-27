use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use ax_sync::SpinLock;

use crate::{CgroupError, CgroupResult, ProcessId, pids::PidsState};

static NEXT_CGROUP_ID: AtomicU64 = AtomicU64::new(2);
const NESTED_CHILDREN_LOCK_SUBCLASS: u32 = 1;
const PIDS_CONTROLLER: &[&str] = &["pids"];
const NO_CONTROLLERS: &[&str] = &[];

/// A stable node in the cgroup v2 hierarchy.
pub struct CgroupNode {
    id: u64,
    name: String,
    parent: Option<Weak<Self>>,
    children: SpinLock<BTreeMap<String, Arc<Self>>>,
    members: SpinLock<BTreeSet<ProcessId>>,
    pids: PidsState,
    pids_enabled_for_children: AtomicBool,
    pins: AtomicUsize,
}

/// An ownership reference that prevents removal of a cgroup hierarchy node.
pub struct CgroupPin {
    node: Arc<CgroupNode>,
}

impl CgroupNode {
    pub(crate) fn new_root() -> Arc<Self> {
        Arc::new(Self {
            id: 1,
            name: String::new(),
            parent: None,
            children: SpinLock::new(BTreeMap::new()),
            members: SpinLock::new(BTreeSet::new()),
            pids: PidsState::new(),
            pids_enabled_for_children: AtomicBool::new(false),
            pins: AtomicUsize::new(0),
        })
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

    /// Return controllers this cgroup may enable for its direct children.
    pub fn available_controllers(&self) -> &'static [&'static str] {
        if self.parent.is_none()
            || self
                .parent()
                .is_some_and(|parent| parent.pids_enabled_for_children.load(Ordering::Acquire))
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

    /// Enable pids for direct children.
    ///
    /// The first controller increment intentionally supports only `+pids`.
    /// Other controller updates remain unsupported until their domain behavior
    /// is implemented and independently validated.
    pub fn write_subtree_control(&self, data: &str) -> CgroupResult<()> {
        if data.trim() != "+pids" || !self.available_controllers().contains(&"pids") {
            return Err(CgroupError::InvalidInput);
        }
        self.pids_enabled_for_children
            .store(true, Ordering::Release);
        Ok(())
    }

    /// Return whether this non-root cgroup exposes pids interface files.
    pub fn has_pids_interface(&self) -> bool {
        self.parent.is_some() && self.available_controllers().contains(&"pids")
    }

    /// Render `pids.max`.
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
        self.require_pids_interface()?;
        self.pids.set_maximum(data)
    }

    /// Create a direct child.
    pub fn create_child(self: &Arc<Self>, name: &str) -> CgroupResult<Arc<Self>> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(CgroupError::InvalidInput);
        }

        let mut children = self.children.lock_irqsave();
        if children.contains_key(name) {
            return Err(CgroupError::AlreadyExists);
        }
        let child = Arc::new(Self {
            id: NEXT_CGROUP_ID.fetch_add(1, Ordering::Relaxed),
            name: name.to_string(),
            parent: Some(Arc::downgrade(self)),
            children: SpinLock::new(BTreeMap::new()),
            members: SpinLock::new(BTreeSet::new()),
            pids: PidsState::new(),
            pids_enabled_for_children: AtomicBool::new(false),
            pins: AtomicUsize::new(0),
        });
        children.insert(name.to_string(), Arc::clone(&child));
        Ok(child)
    }

    /// Look up a direct child.
    pub fn lookup_child(&self, name: &str) -> CgroupResult<Arc<Self>> {
        self.children
            .lock_irqsave()
            .get(name)
            .cloned()
            .ok_or(CgroupError::NotFound)
    }

    /// List direct child names.
    pub fn child_names(&self) -> Vec<String> {
        self.children.lock_irqsave().keys().cloned().collect()
    }

    /// Remove an empty, unpinned direct child.
    pub fn remove_child(&self, name: &str) -> CgroupResult<()> {
        let mut children = self.children.lock_irqsave();
        let child = children.get(name).cloned().ok_or(CgroupError::NotFound)?;
        // Parent and child nodes share the `children` lock class. Hierarchy
        // removal always acquires the direct child's lock below its parent's.
        if !child
            .children
            .lock_irqsave_nested(NESTED_CHILDREN_LOCK_SUBCLASS)
            .is_empty()
        {
            return Err(CgroupError::DirectoryNotEmpty);
        }
        if !child.members.lock_irqsave().is_empty() || child.pins.load(Ordering::Acquire) != 0 {
            return Err(CgroupError::ResourceBusy);
        }
        children.remove(name);
        Ok(())
    }

    /// Return a sorted snapshot of member process IDs.
    pub fn members(&self) -> Vec<ProcessId> {
        self.members.lock_irqsave().iter().copied().collect()
    }

    pub(crate) fn add_member(&self, pid: ProcessId) {
        self.members.lock_irqsave().insert(pid);
    }

    pub(crate) fn remove_member(&self, pid: ProcessId) -> bool {
        self.members.lock_irqsave().remove(&pid)
    }

    pub(crate) fn has_member(&self, pid: ProcessId) -> bool {
        self.members.lock_irqsave().contains(&pid)
    }

    pub(crate) fn try_charge_pids(&self) -> CgroupResult<()> {
        self.pids.try_charge()
    }

    pub(crate) fn charge_pids_unchecked(&self, count: u64) {
        self.pids.charge_unchecked(count);
    }

    pub(crate) fn uncharge_pids(&self, count: u64) {
        self.pids.uncharge(count);
    }

    pub(crate) fn record_pids_max_event(&self) {
        self.pids.record_max_event();
    }

    /// Pin this node as a namespace or mounted hierarchy root.
    pub fn pin(self: &Arc<Self>) -> CgroupPin {
        self.pins.fetch_add(1, Ordering::AcqRel);
        CgroupPin {
            node: Arc::clone(self),
        }
    }

    fn require_pids_interface(&self) -> CgroupResult<()> {
        self.has_pids_interface()
            .then_some(())
            .ok_or(CgroupError::NotFound)
    }
}

impl CgroupPin {
    /// Clone the pinned node handle without creating another logical pin.
    pub fn node(&self) -> Arc<CgroupNode> {
        Arc::clone(&self.node)
    }
}

impl Drop for CgroupPin {
    fn drop(&mut self) {
        let previous = self.node.pins.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "cgroup pin count underflow");
    }
}

fn ancestry(node: &Arc<CgroupNode>) -> Vec<Arc<CgroupNode>> {
    let mut nodes = Vec::new();
    let mut current = Some(Arc::clone(node));
    while let Some(node) = current {
        current = node.parent();
        nodes.push(node);
    }
    nodes
}

pub(crate) fn relative_path(root: &Arc<CgroupNode>, target: &Arc<CgroupNode>) -> String {
    if Arc::ptr_eq(root, target) {
        return "/".to_string();
    }

    let root_path = ancestry(root);
    let target_path = ancestry(target);
    let mut root_unique = root_path.len();
    let mut target_unique = target_path.len();
    while root_unique > 0
        && target_unique > 0
        && Arc::ptr_eq(&root_path[root_unique - 1], &target_path[target_unique - 1])
    {
        root_unique -= 1;
        target_unique -= 1;
    }

    let mut path = String::from("/");
    for index in 0..root_unique {
        if index != 0 {
            path.push('/');
        }
        path.push_str("..");
    }
    for node in target_path[..target_unique].iter().rev() {
        if path != "/" && !path.ends_with('/') {
            path.push('/');
        }
        path.push_str(node.name());
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_paths_relative_to_namespace_root() {
        let root = CgroupNode::new_root();
        let parent = root.create_child("parent").unwrap();
        let child = parent.create_child("child").unwrap();
        let sibling = root.create_child("sibling").unwrap();

        assert_eq!(relative_path(&parent, &parent), "/");
        assert_eq!(relative_path(&parent, &child), "/child");
        assert_eq!(relative_path(&child, &parent), "/..");
        assert_eq!(relative_path(&child, &sibling), "/../../sibling");
    }

    #[test]
    fn rejects_removal_while_node_is_pinned() {
        let root = CgroupNode::new_root();
        let child = root.create_child("child").unwrap();
        let incidental_reference = child.clone();
        let pin = child.pin();
        drop(child);

        assert_eq!(root.remove_child("child"), Err(CgroupError::ResourceBusy));

        drop(pin);
        assert_eq!(root.remove_child("child"), Ok(()));
        assert_eq!(incidental_reference.name(), "child");
    }

    #[test]
    fn exposes_pids_only_after_the_parent_enables_it() {
        let root = CgroupNode::new_root();
        let child = root.create_child("child").unwrap();

        assert_eq!(root.available_controllers(), ["pids"]);
        assert!(!child.has_pids_interface());
        assert_eq!(child.pids_max_text(), Err(CgroupError::NotFound));
        assert_eq!(child.pids_peak_text(), Err(CgroupError::NotFound));

        root.write_subtree_control("+pids").unwrap();

        assert!(child.has_pids_interface());
        assert_eq!(child.pids_max_text(), Ok(String::from("max\n")));
        assert_eq!(child.pids_peak_text(), Ok(String::from("0\n")));
    }

    #[test]
    fn removes_empty_child_from_dynamic_parent() {
        let root = CgroupNode::new_root();
        let parent = root.create_child("parent").unwrap();
        let child = parent.create_child("child").unwrap();
        assert!(parent.child_names().contains(&"child".to_string()));
        assert!(child.child_names().is_empty());

        assert_eq!(parent.remove_child("child"), Ok(()));
    }
}
