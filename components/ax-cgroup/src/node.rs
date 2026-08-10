use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use ax_sync::SpinLock;

use crate::{CgroupError, CgroupResult, ProcessId};

static NEXT_CGROUP_ID: AtomicU64 = AtomicU64::new(2);
const NESTED_CHILDREN_LOCK_SUBCLASS: u32 = 1;

/// A stable node in the cgroup v2 hierarchy.
pub struct CgroupNode {
    id: u64,
    name: String,
    parent: Option<Weak<Self>>,
    children: SpinLock<BTreeMap<String, Arc<Self>>>,
    members: SpinLock<BTreeSet<ProcessId>>,
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

    /// Pin this node as a namespace or mounted hierarchy root.
    pub fn pin(self: &Arc<Self>) -> CgroupPin {
        self.pins.fetch_add(1, Ordering::AcqRel);
        CgroupPin {
            node: Arc::clone(self),
        }
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
    fn removes_empty_child_from_dynamic_parent() {
        let root = CgroupNode::new_root();
        let parent = root.create_child("parent").unwrap();
        let child = parent.create_child("child").unwrap();
        assert!(parent.child_names().contains(&"child".to_string()));
        assert!(child.child_names().is_empty());

        assert_eq!(parent.remove_child("child"), Ok(()));
    }
}
