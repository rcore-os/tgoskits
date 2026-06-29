//! Minimal cgroup v2 compatibility layer.
//!
//! The hierarchy and process membership are real, while controller values are
//! stored as configuration only. This is sufficient for container runtimes
//! that require cgroup v2 bookkeeping, but it does not enforce resource limits.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::fmt::Write;

use ax_errno::{AxError, AxResult, LinuxError};
use ax_kspin::SpinNoIrq;
use spin::LazyLock;
use starry_process::Pid;

use crate::task::{AsThread, get_process_data, get_task, processes};

pub type CgroupId = u64;

const ROOT_ID: CgroupId = 1;
const CONTROLLERS: [&str; 5] = ["cpuset", "cpu", "io", "memory", "pids"];

pub const INTERFACE_FILES: &[&str] = &[
    "cgroup.controllers",
    "cgroup.procs",
    "cgroup.subtree_control",
    "cgroup.threads",
    "cgroup.events",
    "cgroup.type",
    "cgroup.freeze",
    "cgroup.kill",
    "cgroup.stat",
    "cgroup.max.depth",
    "cgroup.max.descendants",
    "cpu.max",
    "cpu.weight",
    "cpu.weight.nice",
    "cpu.stat",
    "cpu.pressure",
    "memory.current",
    "memory.min",
    "memory.low",
    "memory.high",
    "memory.max",
    "memory.swap.current",
    "memory.swap.max",
    "memory.events",
    "memory.events.local",
    "memory.stat",
    "memory.oom.group",
    "memory.pressure",
    "pids.current",
    "pids.max",
    "pids.events",
    "pids.events.local",
    "io.max",
    "io.weight",
    "io.stat",
    "io.pressure",
    "cpuset.cpus",
    "cpuset.cpus.effective",
    "cpuset.mems",
    "cpuset.mems.effective",
    "cpuset.cpus.partition",
];

struct CgroupNode {
    id: CgroupId,
    name: String,
    parent: Option<CgroupId>,
    children: BTreeMap<String, CgroupId>,
    subtree_control: BTreeSet<String>,
    settings: BTreeMap<String, String>,
}

impl CgroupNode {
    fn root() -> Self {
        Self::new(ROOT_ID, None, "")
    }

    fn child(id: CgroupId, parent: CgroupId, name: &str) -> Self {
        Self::new(id, Some(parent), name)
    }

    fn new(id: CgroupId, parent: Option<CgroupId>, name: &str) -> Self {
        Self {
            id,
            name: name.to_string(),
            parent,
            children: BTreeMap::new(),
            subtree_control: BTreeSet::new(),
            settings: default_settings(),
        }
    }
}

struct CgroupTree {
    nodes: BTreeMap<CgroupId, CgroupNode>,
    process_groups: BTreeMap<Pid, CgroupId>,
    next_id: CgroupId,
}

impl CgroupTree {
    fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(ROOT_ID, CgroupNode::root());
        Self {
            nodes,
            process_groups: BTreeMap::new(),
            next_id: ROOT_ID + 1,
        }
    }

    fn descendants(&self, id: CgroupId) -> usize {
        let Some(node) = self.nodes.get(&id) else {
            return 0;
        };
        node.children
            .values()
            .map(|child| 1 + self.descendants(*child))
            .sum()
    }

    fn populated(&self, id: CgroupId) -> bool {
        self.process_groups.values().any(|group| *group == id)
            || self
                .nodes
                .get(&id)
                .is_some_and(|node| node.children.values().any(|child| self.populated(*child)))
    }
}

static CGROUP_TREE: LazyLock<SpinNoIrq<CgroupTree>> =
    LazyLock::new(|| SpinNoIrq::new(CgroupTree::new()));

fn default_settings() -> BTreeMap<String, String> {
    [
        ("cgroup.freeze", "0\n"),
        ("cgroup.max.depth", "max\n"),
        ("cgroup.max.descendants", "max\n"),
        ("cpu.max", "max 100000\n"),
        ("cpu.weight", "100\n"),
        ("cpu.weight.nice", "0\n"),
        ("memory.min", "0\n"),
        ("memory.low", "0\n"),
        ("memory.high", "max\n"),
        ("memory.max", "max\n"),
        ("memory.swap.max", "max\n"),
        ("memory.oom.group", "0\n"),
        ("pids.max", "max\n"),
        ("io.max", "\n"),
        ("io.weight", "default 100\n"),
        ("cpuset.cpus", "\n"),
        ("cpuset.cpus.effective", "0\n"),
        ("cpuset.mems", "\n"),
        ("cpuset.mems.effective", "0\n"),
        ("cpuset.cpus.partition", "member\n"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value.to_string()))
    .collect()
}

pub fn root_id() -> CgroupId {
    ROOT_ID
}

pub fn is_interface_file_name(name: &str) -> bool {
    INTERFACE_FILES.contains(&name)
}

pub fn interface_file_name(name: &str) -> Option<&'static str> {
    INTERFACE_FILES.iter().find(|item| **item == name).copied()
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
    if tree.process_groups.values().any(|group| *group == child_id) {
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

pub fn register_process(pid: Pid, parent_pid: Option<Pid>) {
    let mut tree = CGROUP_TREE.lock();
    let group = parent_pid
        .and_then(|parent| tree.process_groups.get(&parent).copied())
        .filter(|id| tree.nodes.contains_key(id))
        .unwrap_or(ROOT_ID);
    tree.process_groups.insert(pid, group);
}

pub fn unregister_process(pid: Pid) {
    CGROUP_TREE.lock().process_groups.remove(&pid);
}

pub fn process_path(pid: Pid) -> AxResult<String> {
    let id = CGROUP_TREE
        .lock()
        .process_groups
        .get(&pid)
        .copied()
        .unwrap_or(ROOT_ID);
    path(id)
}

pub fn proc_cgroup_text(pid: Pid) -> AxResult<String> {
    Ok(format!("0::{}\n", process_path(pid)?))
}

pub fn procs_text(id: CgroupId) -> AxResult<String> {
    ensure_node_exists(id)?;
    let memberships = CGROUP_TREE.lock().process_groups.clone();
    let mut pids: Vec<_> = processes()
        .into_iter()
        .map(|proc_data| proc_data.proc.pid())
        .filter(|pid| memberships.get(pid).copied().unwrap_or(ROOT_ID) == id)
        .collect();
    pids.sort_unstable();

    let mut text = String::new();
    for pid in pids {
        let _ = writeln!(text, "{pid}");
    }
    Ok(text)
}

fn subtree_control_text(id: CgroupId) -> AxResult<String> {
    let tree = CGROUP_TREE.lock();
    let node = tree.nodes.get(&id).ok_or(AxError::NotFound)?;
    let mut text = node
        .subtree_control
        .iter()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if !text.is_empty() {
        text.push('\n');
    }
    Ok(text)
}

fn process_count(id: CgroupId) -> usize {
    let memberships = CGROUP_TREE.lock().process_groups.clone();
    processes()
        .into_iter()
        .filter(|proc_data| {
            memberships
                .get(&proc_data.proc.pid())
                .copied()
                .unwrap_or(ROOT_ID)
                == id
        })
        .count()
}

pub fn read_interface(id: CgroupId, name: &str) -> AxResult<String> {
    ensure_node_exists(id)?;
    match name {
        "cgroup.controllers" => Ok(format!("{}\n", CONTROLLERS.join(" "))),
        "cgroup.procs" | "cgroup.threads" => procs_text(id),
        "cgroup.subtree_control" => subtree_control_text(id),
        "cgroup.events" => {
            let tree = CGROUP_TREE.lock();
            let populated = usize::from(tree.populated(id));
            let frozen = tree
                .nodes
                .get(&id)
                .and_then(|node| node.settings.get("cgroup.freeze"))
                .is_some_and(|value| value.trim() == "1");
            Ok(format!(
                "populated {populated}\nfrozen {}\n",
                usize::from(frozen)
            ))
        }
        "cgroup.type" => Ok("domain\n".to_string()),
        "cgroup.kill" => Ok(String::new()),
        "cgroup.stat" => {
            let tree = CGROUP_TREE.lock();
            Ok(format!(
                "nr_descendants {}\nnr_dying_descendants 0\n",
                tree.descendants(id)
            ))
        }
        "pids.current" => Ok(format!("{}\n", process_count(id))),
        "pids.events" | "pids.events.local" => Ok("max 0\n".to_string()),
        "memory.current" | "memory.swap.current" => Ok("0\n".to_string()),
        "memory.events" | "memory.events.local" => {
            Ok("low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\n".to_string())
        }
        "memory.stat" => Ok("anon 0\nfile 0\nkernel 0\nsock 0\n".to_string()),
        "cpu.stat" => Ok("usage_usec 0\nuser_usec 0\nsystem_usec 0\n".to_string()),
        "cpu.pressure" | "memory.pressure" | "io.pressure" => Ok("some avg10=0.00 avg60=0.00 \
                                                                  avg300=0.00 total=0\nfull \
                                                                  avg10=0.00 avg60=0.00 \
                                                                  avg300=0.00 total=0\n"
            .to_string()),
        "io.stat" => Ok(String::new()),
        _ => {
            let tree = CGROUP_TREE.lock();
            tree.nodes
                .get(&id)
                .and_then(|node| node.settings.get(name))
                .cloned()
                .ok_or(AxError::NotFound)
        }
    }
}

fn move_process(id: CgroupId, data: &[u8], thread: bool) -> AxResult<()> {
    ensure_node_exists(id)?;
    let input = core::str::from_utf8(data)
        .map_err(|_| AxError::InvalidInput)?
        .trim();
    let requested = input.parse::<Pid>().map_err(|_| AxError::InvalidInput)?;
    let pid = if requested == 0 {
        ax_task::current().as_thread().proc_data.proc.pid()
    } else if thread {
        get_task(requested)?.as_thread().proc_data.proc.pid()
    } else {
        get_process_data(requested)?.proc.pid()
    };
    CGROUP_TREE.lock().process_groups.insert(pid, id);
    Ok(())
}

fn write_subtree_control(id: CgroupId, data: &[u8]) -> AxResult<()> {
    let input = core::str::from_utf8(data).map_err(|_| AxError::InvalidInput)?;
    let mut tree = CGROUP_TREE.lock();
    let node = tree.nodes.get_mut(&id).ok_or(AxError::NotFound)?;
    for command in input.split_ascii_whitespace() {
        let (enable, controller) = match command.as_bytes().first() {
            Some(b'+') => (true, &command[1..]),
            Some(b'-') => (false, &command[1..]),
            _ => return Err(AxError::InvalidInput),
        };
        if !CONTROLLERS.contains(&controller) {
            return Err(AxError::InvalidInput);
        }
        if enable {
            node.subtree_control.insert(controller.to_string());
        } else {
            node.subtree_control.remove(controller);
        }
    }
    Ok(())
}

fn write_setting(id: CgroupId, name: &str, data: &[u8]) -> AxResult<()> {
    let value = core::str::from_utf8(data)
        .map_err(|_| AxError::InvalidInput)?
        .trim();
    if value.is_empty() && !matches!(name, "io.max" | "cpuset.cpus" | "cpuset.mems") {
        return Err(AxError::InvalidInput);
    }
    if name == "cgroup.freeze" && !matches!(value, "0" | "1") {
        return Err(AxError::InvalidInput);
    }

    let mut stored = value.to_string();
    stored.push('\n');
    let mut tree = CGROUP_TREE.lock();
    let node = tree.nodes.get_mut(&id).ok_or(AxError::NotFound)?;
    let setting = node.settings.get_mut(name).ok_or(AxError::NotFound)?;
    *setting = stored;
    Ok(())
}

pub fn write_interface(id: CgroupId, name: &str, data: &[u8]) -> AxResult<()> {
    match name {
        "cgroup.procs" => move_process(id, data, false),
        "cgroup.threads" => move_process(id, data, true),
        "cgroup.subtree_control" => write_subtree_control(id, data),
        // Docker/runc use cgroup.kill during cleanup. Resource enforcement is
        // intentionally absent in this compatibility layer, so accept it.
        "cgroup.kill" => Ok(()),
        "cgroup.controllers"
        | "cgroup.events"
        | "cgroup.type"
        | "cgroup.stat"
        | "cpu.stat"
        | "cpu.pressure"
        | "memory.current"
        | "memory.swap.current"
        | "memory.events"
        | "memory.events.local"
        | "memory.stat"
        | "memory.pressure"
        | "pids.current"
        | "pids.events"
        | "pids.events.local"
        | "io.stat"
        | "io.pressure"
        | "cpuset.cpus.effective"
        | "cpuset.mems.effective" => Err(AxError::from(LinuxError::EACCES)),
        _ if is_interface_file_name(name) => write_setting(id, name, data),
        _ => Err(AxError::NotFound),
    }
}

pub fn ensure_node_exists(id: CgroupId) -> AxResult<()> {
    let tree = CGROUP_TREE.lock();
    let node = tree.nodes.get(&id).ok_or(AxError::NotFound)?;
    debug_assert_eq!(node.id, id);
    Ok(())
}
