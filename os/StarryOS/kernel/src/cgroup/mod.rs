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
    live_processes: usize,
}

impl CgroupNode {
    fn root() -> Self {
        Self {
            id: ROOT_ID,
            name: String::new(),
            parent: None,
            children: BTreeMap::new(),
            live_processes: 0,
        }
    }

    fn child(id: CgroupId, parent: CgroupId, name: &str) -> Self {
        Self {
            id,
            name: name.to_string(),
            parent: Some(parent),
            children: BTreeMap::new(),
            live_processes: 0,
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
    let child = tree.nodes.get(&child_id).ok_or(AxError::NotFound)?;
    debug_assert_eq!(child.id, child_id);
    if !child.children.is_empty() {
        return Err(AxError::DirectoryNotEmpty);
    }
    if child.live_processes != 0 {
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

pub fn register_process(id: CgroupId) -> AxResult<()> {
    let mut tree = CGROUP_TREE.lock();
    let node = tree.nodes.get_mut(&id).ok_or(AxError::NotFound)?;
    node.live_processes = node
        .live_processes
        .checked_add(1)
        .ok_or_else(|| AxError::from(LinuxError::EINVAL))?;
    Ok(())
}

pub fn register_fork_child(parent: &crate::task::ProcessData) -> AxResult<CgroupId> {
    let mut tree = CGROUP_TREE.lock();
    if !parent.is_cgroup_membership_active() {
        return Err(AxError::from(LinuxError::ESRCH));
    }

    let id = parent.cgroup_id();
    let node = tree
        .nodes
        .get_mut(&id)
        .ok_or_else(|| AxError::from(LinuxError::ESRCH))?;
    node.live_processes = node
        .live_processes
        .checked_add(1)
        .ok_or_else(|| AxError::from(LinuxError::EINVAL))?;
    Ok(id)
}

fn unregister_process_locked(tree: &mut CgroupTree, id: CgroupId) {
    if let Some(node) = tree.nodes.get_mut(&id) {
        if let Some(live_processes) = node.live_processes.checked_sub(1) {
            node.live_processes = live_processes;
        } else {
            debug_assert!(false, "cgroup live_processes underflow during unregister");
        }
    }
}

pub fn release_process_membership(proc_data: &crate::task::ProcessData) {
    let mut tree = CGROUP_TREE.lock();
    if proc_data.deactivate_cgroup_membership() {
        unregister_process_locked(&mut tree, proc_data.cgroup_id());
    }
}

pub fn attach_process(target: CgroupId, pid: Pid) -> AxResult<()> {
    if pid == 0 {
        return Err(AxError::from(LinuxError::EINVAL));
    }

    let proc_data =
        crate::task::get_process_data(pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;

    let mut tree = CGROUP_TREE.lock();
    if !tree.nodes.contains_key(&target) {
        return Err(AxError::NotFound);
    }
    if !proc_data.is_cgroup_membership_active() {
        return Err(AxError::from(LinuxError::ESRCH));
    }

    let old = proc_data.cgroup_id();
    if !tree.nodes.contains_key(&old) {
        return Err(AxError::NotFound);
    }
    if old == target {
        return Ok(());
    }

    let target_live_processes = tree
        .nodes
        .get(&target)
        .expect("target was checked above")
        .live_processes
        .checked_add(1)
        .ok_or_else(|| AxError::from(LinuxError::EINVAL))?;
    let old_live_processes = tree
        .nodes
        .get(&old)
        .expect("old cgroup was checked above")
        .live_processes
        .checked_sub(1)
        .ok_or_else(|| AxError::from(LinuxError::EINVAL))?;
    tree.nodes
        .get_mut(&old)
        .expect("old cgroup was checked above")
        .live_processes = old_live_processes;
    tree.nodes
        .get_mut(&target)
        .expect("target was checked above")
        .live_processes = target_live_processes;
    proc_data.set_cgroup_id(target);
    Ok(())
}

fn path_locked(tree: &CgroupTree, id: CgroupId) -> AxResult<String> {
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

pub fn path(id: CgroupId) -> AxResult<String> {
    let tree = CGROUP_TREE.lock();
    path_locked(&tree, id)
}

pub fn procs_text(id: CgroupId) -> AxResult<String> {
    ensure_node_exists(id)?;

    let mut pids: Vec<_> = crate::task::processes()
        .into_iter()
        .filter(|proc_data| proc_data.is_cgroup_membership_active() && proc_data.cgroup_id() == id)
        .map(|proc_data| proc_data.proc.pid())
        .collect();
    pids.sort_unstable();

    let mut text = String::new();
    for pid in pids {
        let _ = writeln!(text, "{pid}");
    }
    Ok(text)
}

pub fn proc_cgroup_text(proc_data: &crate::task::ProcessData) -> AxResult<String> {
    let tree = CGROUP_TREE.lock();
    if !proc_data.is_cgroup_membership_active() {
        return Err(AxError::from(LinuxError::ESRCH));
    }
    let path = path_locked(&tree, proc_data.cgroup_id())?;
    let mut text = String::new();
    let _ = writeln!(text, "0::{path}");
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
    let pid = parse_procs_pid(data)?;
    attach_process(id, pid)
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

pub fn parse_procs_pid(data: &[u8]) -> AxResult<Pid> {
    let data = trim_ascii_whitespace(data);
    if data.is_empty() {
        return Err(AxError::from(LinuxError::EINVAL));
    }

    let mut value = 0u64;
    for byte in data {
        if !byte.is_ascii_digit() {
            return Err(AxError::from(LinuxError::EINVAL));
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            .ok_or_else(|| AxError::from(LinuxError::EINVAL))?;
        if value > u64::from(Pid::MAX) {
            return Err(AxError::from(LinuxError::EINVAL));
        }
    }

    if value == 0 {
        return Err(AxError::from(LinuxError::EINVAL));
    }
    Ok(value as Pid)
}

fn trim_ascii_whitespace(data: &[u8]) -> &[u8] {
    let start = data
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(data.len());
    let end = data
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &data[start..end]
}
