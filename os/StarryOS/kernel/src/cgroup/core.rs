//! cgroup v2 core data structures.

use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    sync::{Arc, Weak},
    vec::Vec,
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{VfsError, VfsResult};

use super::{cpu::CpuState, pids::PidsState};

/// A cgroup node in the hierarchy.
#[allow(dead_code)]
pub struct CgroupNode {
    /// Directory name (e.g. "my-cgroup").
    pub name: String,
    /// Full path from root (e.g. "/my-cgroup").
    pub path: String,
    /// Child cgroups.
    pub children: SpinNoIrq<BTreeMap<String, Arc<CgroupNode>>>,
    /// PIDs in this cgroup.
    pub procs: SpinNoIrq<Vec<u32>>,
    /// Registered controller names (e.g. "pids", "cpu").
    pub controllers: Vec<String>,
    /// Parent (None for root).
    pub parent: Option<Weak<CgroupNode>>,
    /// Pids controller state.
    pub pids: Arc<PidsState>,
    pub cpu: Arc<CpuState>,
}

impl CgroupNode {
    fn new_root() -> Arc<Self> {
        Arc::new(Self {
            name: String::new(),
            path: "/".to_string(),
            children: SpinNoIrq::new(BTreeMap::new()),
            procs: SpinNoIrq::new(Vec::new()),
            controllers: Vec::new(),
            parent: None,
            pids: Arc::new(PidsState::new()),
            cpu: Arc::new(CpuState::new()),
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
        let child = Arc::new(CgroupNode {
            name: name.to_string(),
            path: child_path,
            children: SpinNoIrq::new(BTreeMap::new()),
            procs: SpinNoIrq::new(Vec::new()),
            controllers: Vec::new(),
            parent: Some(Arc::downgrade(self)),
            pids: Arc::new(PidsState::new()),
            cpu: Arc::new(CpuState::new()),
        });
        children.insert(name.to_string(), child);
        Ok(children.get(name).unwrap().clone())
    }

    /// List controller names.
    pub fn controller_list(&self) -> String {
        let mut list = alloc::vec!["pids".to_string(), "cpu".to_string()];
        list.extend(self.controllers.iter().cloned());
        list.join(" ")
    }
}

/// Global cgroup v2 root.
pub static GLOBAL_CGROUP_ROOT: LazyInit<Arc<CgroupNode>> = LazyInit::new();

pub fn init() {
    GLOBAL_CGROUP_ROOT.init_once(CgroupNode::new_root());
}
