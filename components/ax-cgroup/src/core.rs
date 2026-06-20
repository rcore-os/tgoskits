//! cgroup v2 core data structures.

use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};

use ::core::sync::atomic::{AtomicU64, Ordering};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{VfsError, VfsResult};

use super::{CgroupId, ROOT_ID, controller, pids::PidsState};

static NEXT_CGROUP_ID: AtomicU64 = AtomicU64::new(ROOT_ID + 1);
static CGROUP_REGISTRY: LazyInit<SpinNoIrq<BTreeMap<CgroupId, Weak<CgroupNode>>>> = LazyInit::new();

/// A cgroup node in the hierarchy.
pub struct CgroupNode {
    /// Stable cgroup id used by cgroupfs VFS entries.
    pub id: CgroupId,
    /// Directory name (e.g. "my-cgroup").
    pub name: String,
    /// Full path from root (e.g. "/my-cgroup").
    pub path: String,
    /// Child cgroups.
    pub children: SpinNoIrq<BTreeMap<String, Arc<CgroupNode>>>,
    /// PIDs in this cgroup.
    pub procs: SpinNoIrq<Vec<u32>>,
    /// Dynamic controller instances keyed by controller name.
    pub controllers: BTreeMap<String, Arc<dyn controller::CgroupController>>,
    /// Controllers enabled for child cgroups via `cgroup.subtree_control`.
    pub subtree_control: SpinNoIrq<Vec<String>>,
    /// Parent (None for root).
    pub parent: Option<Weak<CgroupNode>>,
    /// Pids controller state — fast path for fork charge/uncharge.
    pub pids: Arc<PidsState>,
}

impl CgroupNode {
    pub fn new_root() -> Arc<Self> {
        // Create all registered controller instances for root
        let mut controllers = BTreeMap::new();
        for name in controller::all_factory_names() {
            if let Some(factory) = controller::get_factory(&name) {
                controllers.insert(name, factory.new_instance());
            }
        }

        // Extract pids state for fast-path access
        let pids = controllers
            .get("pids")
            .and_then(|ctrl| ctrl.as_any().downcast_ref::<super::pids::PidsController>())
            .map(|ctrl| ctrl.state().clone())
            .unwrap_or_else(|| Arc::new(PidsState::new()));

        Arc::new(Self {
            id: ROOT_ID,
            name: String::new(),
            path: "/".to_string(),
            children: SpinNoIrq::new(BTreeMap::new()),
            procs: SpinNoIrq::new(Vec::new()),
            controllers,
            subtree_control: SpinNoIrq::new(Vec::new()),
            parent: None,
            pids,
        })
    }

    /// Create a child cgroup under this node.
    pub fn create_child(self: &Arc<Self>, name: &str) -> VfsResult<Arc<CgroupNode>> {
        let mut children = self.children.lock();
        if children.contains_key(name) {
            return Err(VfsError::AlreadyExists);
        }
        let child_path = if self.path == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", self.path, name)
        };
        let id = NEXT_CGROUP_ID.fetch_add(1, Ordering::AcqRel);

        // Create controllers based on parent's subtree_control
        let subtree_ctrl = self.subtree_control.lock();
        let mut controllers = BTreeMap::new();
        for ctrl_name in subtree_ctrl.iter() {
            if let Some(factory) = controller::get_factory(ctrl_name) {
                controllers.insert(ctrl_name.clone(), factory.new_instance());
            }
        }
        // pids always present (needed for fork charge)
        if !controllers.contains_key("pids")
            && let Some(factory) = controller::get_factory("pids")
        {
            controllers.insert("pids".to_string(), factory.new_instance());
        }

        let pids = controllers
            .get("pids")
            .and_then(|ctrl| ctrl.as_any().downcast_ref::<super::pids::PidsController>())
            .map(|ctrl| ctrl.state().clone())
            .unwrap_or_else(|| Arc::new(PidsState::new()));

        let child = Arc::new(CgroupNode {
            id,
            name: name.to_string(),
            path: child_path,
            children: SpinNoIrq::new(BTreeMap::new()),
            procs: SpinNoIrq::new(Vec::new()),
            controllers,
            subtree_control: SpinNoIrq::new(Vec::new()),
            parent: Some(Arc::downgrade(self)),
            pids,
        });

        // Inherit cpuset masks from parent
        if let Some(parent_cpuset) = self.controllers.get("cpuset")
            && let Some(parent_ctrl) = parent_cpuset
                .as_any()
                .downcast_ref::<super::cpuset::CpusetController>()
            && let Some(child_cpuset) = child.controllers.get("cpuset")
            && let Some(child_ctrl) = child_cpuset
                .as_any()
                .downcast_ref::<super::cpuset::CpusetController>()
        {
            let parent_cpus = parent_ctrl.state().cpus.load(Ordering::Acquire);
            let parent_mems = parent_ctrl.state().mems.load(Ordering::Acquire);
            child_ctrl
                .state()
                .cpus
                .store(parent_cpus, Ordering::Release);
            child_ctrl
                .state()
                .mems
                .store(parent_mems, Ordering::Release);
            child_ctrl
                .state()
                .cpus_effective
                .store(parent_cpus, Ordering::Release);
            child_ctrl
                .state()
                .mems_effective
                .store(parent_mems, Ordering::Release);
        }

        children.insert(name.to_string(), child.clone());
        register_node(&child);
        Ok(child)
    }

    /// List controller names available to this node.
    pub fn controller_list(&self) -> String {
        if self.id == ROOT_ID {
            self.controllers
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            self.parent
                .as_ref()
                .and_then(Weak::upgrade)
                .map(|parent| parent.subtree_control.lock().join(" "))
                .unwrap_or_default()
        }
    }
}

/// Global cgroup v2 root.
pub static GLOBAL_CGROUP_ROOT: LazyInit<Arc<CgroupNode>> = LazyInit::new();

pub fn init() {
    CGROUP_REGISTRY.init_once(SpinNoIrq::new(BTreeMap::new()));
    GLOBAL_CGROUP_ROOT.init_once(CgroupNode::new_root());
    register_node(GLOBAL_CGROUP_ROOT.get().expect("cgroup root initialized"));
}

pub fn root_id() -> CgroupId {
    ROOT_ID
}

pub fn register_node(node: &Arc<CgroupNode>) {
    if let Some(registry) = CGROUP_REGISTRY.get() {
        registry.lock().insert(node.id, Arc::downgrade(node));
    }
}

fn unregister_node(id: CgroupId) {
    if let Some(registry) = CGROUP_REGISTRY.get() {
        registry.lock().remove(&id);
    }
}

fn unregister_subtree(node: &Arc<CgroupNode>) {
    let children = node.children.lock().values().cloned().collect::<Vec<_>>();
    for child in children {
        unregister_subtree(&child);
    }
    unregister_node(node.id);
}

pub fn get_node(id: CgroupId) -> VfsResult<Arc<CgroupNode>> {
    let registry = CGROUP_REGISTRY.get().ok_or(VfsError::NotFound)?;
    registry
        .lock()
        .get(&id)
        .and_then(Weak::upgrade)
        .ok_or(VfsError::NotFound)
}

pub fn path(id: CgroupId) -> VfsResult<String> {
    Ok(get_node(id)?.path.clone())
}

pub fn child_names(id: CgroupId) -> VfsResult<Vec<String>> {
    Ok(get_node(id)?.children.lock().keys().cloned().collect())
}

pub fn lookup_child(parent_id: CgroupId, name: &str) -> VfsResult<CgroupId> {
    get_node(parent_id)?
        .children
        .lock()
        .get(name)
        .map(|child| child.id)
        .ok_or(VfsError::NotFound)
}

pub fn create_child(parent_id: CgroupId, name: &str) -> VfsResult<CgroupId> {
    Ok(get_node(parent_id)?.create_child(name)?.id)
}

pub fn remove_child(parent_id: CgroupId, name: &str) -> VfsResult<()> {
    let parent = get_node(parent_id)?;
    let mut children = parent.children.lock();
    let child = children.get(name).cloned().ok_or(VfsError::NotFound)?;
    if !child.children.lock().is_empty() {
        return Err(VfsError::DirectoryNotEmpty);
    }
    if !child.procs.lock().is_empty() {
        return Err(VfsError::ResourceBusy);
    }
    children.remove(name);
    unregister_subtree(&child);
    Ok(())
}
