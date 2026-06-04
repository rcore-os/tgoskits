use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt::Write;

use ax_errno::{AxError, AxResult, LinuxError};
use ax_kspin::SpinNoIrq;
use spin::LazyLock;
use starry_process::Pid;

pub type CgroupId = u64;

const ROOT_ID: CgroupId = 1;
const INTERFACE_FILES: [&str; 3] = [
    "cgroup.procs",
    "cgroup.controllers",
    "cgroup.subtree_control",
];

struct CgroupNode {
    id: CgroupId,
    name: String,
    parent: Option<CgroupId>,
    children: BTreeMap<String, CgroupId>,
}

impl CgroupNode {
    fn root() -> Self {
        Self {
            id: ROOT_ID,
            name: String::new(),
            parent: None,
            children: BTreeMap::new(),
        }
    }

    fn child(id: CgroupId, parent: CgroupId, name: &str) -> Self {
        Self {
            id,
            name: name.to_string(),
            parent: Some(parent),
            children: BTreeMap::new(),
        }
    }
}

struct CgroupTree {
    nodes: BTreeMap<CgroupId, CgroupNode>,
    next_id: CgroupId,
}

impl CgroupTree {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT_ID, CgroupNode::root());
        Self {
            nodes,
            next_id: ROOT_ID + 1,
        }
    }
}

static CGROUP_TREE: LazyLock<SpinNoIrq<CgroupTree>> =
    LazyLock::new(|| SpinNoIrq::new(CgroupTree::new()));

pub fn root_id() -> CgroupId {
    ROOT_ID
}

pub fn is_interface_file_name(name: &str) -> bool {
    INTERFACE_FILES.contains(&name)
}

pub fn child_names(parent: CgroupId) -> AxResult<Vec<String>> {
    let tree = CGROUP_TREE.lock();
    let node = tree.nodes.get(&parent).ok_or(AxError::NotFound)?;
    debug_assert_eq!(node.id, parent);
    Ok(node.children.keys().cloned().collect())
}

pub fn lookup_child(parent: CgroupId, name: &str) -> AxResult<CgroupId> {
    let tree = CGROUP_TREE.lock();
    let node = tree.nodes.get(&parent).ok_or(AxError::NotFound)?;
    debug_assert_eq!(node.id, parent);
    node.children.get(name).copied().ok_or(AxError::NotFound)
}

pub fn create_child(parent: CgroupId, name: &str) -> AxResult<CgroupId> {
    if name.is_empty() {
        return Err(AxError::InvalidInput);
    }
    if is_interface_file_name(name) {
        return Err(AxError::AlreadyExists);
    }

    let mut tree = CGROUP_TREE.lock();
    {
        let parent_node = tree.nodes.get(&parent).ok_or(AxError::NotFound)?;
        debug_assert_eq!(parent_node.id, parent);
        if parent_node.children.contains_key(name) {
            return Err(AxError::AlreadyExists);
        }
    }

    let id = tree.next_id;
    tree.next_id = id.checked_add(1).ok_or(AxError::NoMemory)?;
    tree.nodes.insert(id, CgroupNode::child(id, parent, name));
    tree.nodes
        .get_mut(&parent)
        .expect("parent was checked above")
        .children
        .insert(name.to_string(), id);
    Ok(id)
}

pub fn remove_child(parent: CgroupId, name: &str) -> AxResult<()> {
    if name.is_empty() {
        return Err(AxError::InvalidInput);
    }

    let mut tree = CGROUP_TREE.lock();
    let child_id = {
        let parent_node = tree.nodes.get(&parent).ok_or(AxError::NotFound)?;
        debug_assert_eq!(parent_node.id, parent);
        parent_node
            .children
            .get(name)
            .copied()
            .ok_or(AxError::NotFound)?
    };
    {
        let child = tree.nodes.get(&child_id).ok_or(AxError::NotFound)?;
        debug_assert_eq!(child.id, child_id);
        if !child.children.is_empty() {
            return Err(AxError::DirectoryNotEmpty);
        }
    }
    // A cgroup with live member processes is busy. Membership is derived by
    // scanning live processes for this id.
    if crate::task::processes()
        .iter()
        .any(|proc_data| proc_data.cgroup_id() == child_id)
    {
        return Err(AxError::ResourceBusy);
    }

    tree.nodes
        .get_mut(&parent)
        .expect("parent was checked above")
        .children
        .remove(name);
    tree.nodes.remove(&child_id);
    Ok(())
}

pub fn path(id: CgroupId) -> AxResult<String> {
    let tree = CGROUP_TREE.lock();
    let mut current = id;
    let mut names = Vec::new();
    loop {
        let node = tree.nodes.get(&current).ok_or(AxError::NotFound)?;
        debug_assert_eq!(node.id, current);
        if let Some(parent) = node.parent {
            names.push(node.name.clone());
            current = parent;
        } else {
            break;
        }
    }

    if names.is_empty() {
        return Ok("/".to_string());
    }

    names.reverse();
    let mut path = String::new();
    for name in names {
        path.push('/');
        path.push_str(&name);
    }
    Ok(path)
}

pub fn procs_text(id: CgroupId) -> AxResult<String> {
    ensure_node_exists(id)?;

    let mut pids: Vec<_> = crate::task::processes()
        .into_iter()
        .filter(|proc_data| proc_data.cgroup_id() == id)
        .map(|proc_data| proc_data.proc.pid())
        .collect();
    pids.sort_unstable();

    let mut text = String::new();
    for pid in pids {
        let _ = writeln!(text, "{pid}");
    }
    Ok(text)
}

pub fn controllers_text(id: CgroupId) -> AxResult<&'static str> {
    ensure_node_exists(id)?;
    Ok("")
}

pub fn subtree_control_text(id: CgroupId) -> AxResult<&'static str> {
    ensure_node_exists(id)?;
    Ok("")
}

pub fn write_procs(id: CgroupId, data: &[u8]) -> AxResult<()> {
    ensure_node_exists(id)?;

    let text = core::str::from_utf8(data).map_err(|_| AxError::InvalidInput)?;
    let pid: i32 = text.trim().parse().map_err(|_| AxError::InvalidInput)?;
    if pid < 0 {
        return Err(AxError::InvalidInput);
    }

    let proc_data = crate::task::get_process_data(pid as Pid)?;
    proc_data.set_cgroup_id(id);
    Ok(())
}

pub fn write_subtree_control(id: CgroupId, _data: &[u8]) -> AxResult<()> {
    ensure_node_exists(id)?;
    Err(AxError::from(LinuxError::EINVAL))
}

pub fn ensure_node_exists(id: CgroupId) -> AxResult<()> {
    let tree = CGROUP_TREE.lock();
    let node = tree.nodes.get(&id).ok_or(AxError::NotFound)?;
    debug_assert_eq!(node.id, id);
    Ok(())
}
