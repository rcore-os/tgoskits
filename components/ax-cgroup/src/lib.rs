//! cgroup v2 subsystem for StarryOS.
//!
//! This crate provides the core cgroup v2 hierarchy, controller state,
//! and membership management. It is kernel-independent — the kernel
//! provides a [`CgroupProvider`] implementation to supply task/process
//! primitives.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod controller;
mod core;
pub mod cpu;
pub mod cpuset;
pub mod io;
pub mod memory;
pub mod pids;
pub mod provider;

#[cfg(test)]
mod tests;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
pub use core::{CgroupNode, GLOBAL_CGROUP_ROOT};

use ::core::{fmt::Write, str};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use axfs_ng_vfs::{VfsError, VfsResult};
#[allow(unused_imports)]
use log::info;
pub use provider::CgroupProvider;

pub type CgroupId = u64;
pub const ROOT_ID: CgroupId = 1;

const BUILTIN_FILES: &[&str] = &[
    "cgroup.controllers",
    "cgroup.procs",
    "cgroup.subtree_control",
    "cgroup.type",
];

struct MembershipState {
    detached_pids: BTreeSet<u32>,
    pending_pids: BTreeMap<u32, CgroupId>,
}

static MEMBERSHIP: LazyInit<SpinNoIrq<MembershipState>> = LazyInit::new();
static PROVIDER: LazyInit<provider::ProviderCell> = LazyInit::new();

/// Initialize the cgroup subsystem. Called once during boot.
pub fn init() {
    controller::init_registry();

    MEMBERSHIP.init_once(SpinNoIrq::new(MembershipState {
        detached_pids: BTreeSet::new(),
        pending_pids: BTreeMap::new(),
    }));
    PROVIDER.init_once(provider::ProviderCell::new());

    // Register all controller factories
    controller::register_factory(Arc::new(pids::PidsControllerFactory));
    controller::register_factory(Arc::new(cpu::CpuControllerFactory));
    controller::register_factory(Arc::new(cpuset::CpusetControllerFactory));
    controller::register_factory(Arc::new(memory::MemoryControllerFactory));
    controller::register_factory(Arc::new(io::IoControllerFactory));

    core::init();
    info!("cgroup: initialized with 5 controllers");
}

/// Register the kernel provider. Must be called after [`init`].
pub fn register_provider(provider: &'static dyn CgroupProvider) {
    PROVIDER
        .get()
        .expect("cgroup not initialized")
        .set(provider);
}

fn with_provider<F, R>(f: F) -> VfsResult<R>
where
    F: FnOnce(&dyn CgroupProvider) -> VfsResult<R>,
{
    let cell = PROVIDER.get().ok_or(VfsError::BadState)?;
    let provider = cell.get().ok_or(VfsError::BadState)?;
    f(provider)
}

pub fn root_id() -> CgroupId {
    core::root_id()
}

pub fn path(id: CgroupId) -> VfsResult<String> {
    core::path(id)
}

pub fn ensure_node_exists(id: CgroupId) -> VfsResult<()> {
    core::get_node(id).map(|_| ())
}

pub fn child_names(id: CgroupId) -> VfsResult<Vec<String>> {
    core::child_names(id)
}

pub fn lookup_child(parent_id: CgroupId, name: &str) -> VfsResult<CgroupId> {
    core::lookup_child(parent_id, name)
}

pub fn create_child(parent_id: CgroupId, name: &str) -> VfsResult<CgroupId> {
    let _membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();
    if is_interface_file_name(name) || is_controller_attr(parent_id, name)? {
        return Err(VfsError::AlreadyExists);
    }
    core::create_child(parent_id, name)
}

pub fn remove_child(parent_id: CgroupId, name: &str) -> VfsResult<()> {
    let membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();
    let child_id = core::lookup_child(parent_id, name)?;
    if pending_count_in_node(&membership, child_id) != 0 {
        return Err(VfsError::ResourceBusy);
    }
    core::remove_child(parent_id, name)
}

pub fn controllers_text(id: CgroupId) -> VfsResult<String> {
    Ok(core::get_node(id)?.controller_list())
}

pub fn procs_text(id: CgroupId) -> VfsResult<String> {
    let node = core::get_node(id)?;
    let mut text = String::new();
    for pid in node.procs.lock().iter() {
        let _ = writeln!(text, "{}", pid);
    }
    Ok(text)
}

pub fn subtree_control_text(id: CgroupId) -> VfsResult<String> {
    let node = core::get_node(id)?;
    Ok(node.subtree_control.lock().join(" "))
}

pub fn write_subtree_control(id: CgroupId, data: &[u8]) -> VfsResult<()> {
    let membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();
    let node = core::get_node(id)?;
    let text = str::from_utf8(data)
        .map_err(|_| VfsError::InvalidInput)?
        .trim();
    let mut next = node.subtree_control.lock().clone();
    for part in text.split_whitespace() {
        if let Some(name) = part.strip_prefix('+') {
            if !controller_available(&node, name) {
                return Err(VfsError::InvalidInput);
            }
            if is_domain_controller(name)
                && node.id != root_id()
                && node_has_processes_or_pending(&membership, &node)
            {
                return Err(VfsError::ResourceBusy);
            }
            if !next.iter().any(|c| c == name) {
                next.push(name.to_string());
            }
        } else if let Some(name) = part.strip_prefix('-') {
            if !controller_available(&node, name) {
                return Err(VfsError::InvalidInput);
            }
            next.retain(|c| c != name);
        } else {
            return Err(VfsError::InvalidInput);
        }
    }
    next.sort();
    *node.subtree_control.lock() = next;
    Ok(())
}

pub fn write_procs(id: CgroupId, data: &[u8]) -> VfsResult<()> {
    let text = str::from_utf8(data)
        .map_err(|_| VfsError::InvalidInput)?
        .trim();
    let pid: u32 = text.parse().map_err(|_| VfsError::InvalidInput)?;
    migrate_process(pid, id)
}

// ── Attribute dispatch (unified via controller trait) ─────────────────

pub fn all_attr_names(id: CgroupId) -> VfsResult<Vec<String>> {
    let node = core::get_node(id)?;
    let mut names = Vec::new();
    for (ctrl_name, ctrl) in node.controllers.iter() {
        if controller_available(&node, ctrl_name) {
            for attr in ctrl.attr_names() {
                names.push(format!("{}.{}", ctrl_name, attr.name));
            }
        }
    }
    Ok(names)
}

pub fn is_controller_attr(id: CgroupId, name: &str) -> VfsResult<bool> {
    let node = core::get_node(id)?;
    let (ctrl_name, attr_name) = match controller::parse_attr_name(name) {
        Some(pair) => pair,
        None => return Ok(false),
    };
    if !controller_available(&node, ctrl_name) {
        return Ok(false);
    }
    if let Some(ctrl) = node.controllers.get(ctrl_name) {
        Ok(ctrl.attr_names().iter().any(|a| a.name == attr_name))
    } else {
        Ok(false)
    }
}

pub fn attr_is_read_only(id: CgroupId, name: &str) -> VfsResult<Option<bool>> {
    let node = core::get_node(id)?;
    let (ctrl_name, attr_name) = match controller::parse_attr_name(name) {
        Some(pair) => pair,
        None => return Ok(None),
    };
    if let Some(ctrl) = node.controllers.get(ctrl_name) {
        Ok(ctrl
            .attr_names()
            .iter()
            .find(|a| a.name == attr_name)
            .map(|a| a.read_only))
    } else {
        Ok(None)
    }
}

pub fn is_interface_file_name(name: &str) -> bool {
    if BUILTIN_FILES.contains(&name) {
        return true;
    }
    // Any "controller.attr" pattern is reserved
    controller::parse_attr_name(name).is_some()
}

pub fn read_attr_at(id: CgroupId, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
    let node = core::get_node(id)?;
    let (ctrl_name, attr_name) = controller::parse_attr_name(name).ok_or(VfsError::NotFound)?;
    if !controller_available(&node, ctrl_name) {
        return Err(VfsError::NotFound);
    }
    let ctrl = node.controllers.get(ctrl_name).ok_or(VfsError::NotFound)?;
    ctrl.read_attr(attr_name, offset, buf)
}

pub fn write_attr(id: CgroupId, name: &str, data: &[u8]) -> VfsResult<usize> {
    let node = core::get_node(id)?;
    let (ctrl_name, attr_name) = controller::parse_attr_name(name).ok_or(VfsError::NotFound)?;
    if !controller_available(&node, ctrl_name) {
        return Err(VfsError::NotFound);
    }
    let ctrl = node.controllers.get(ctrl_name).ok_or(VfsError::NotFound)?;
    // Check read-only
    if ctrl
        .attr_names()
        .iter()
        .any(|a| a.name == attr_name && a.read_only)
    {
        return Err(VfsError::OperationNotPermitted);
    }
    ctrl.write_attr(attr_name, data)
}

// ── Process membership ───────────────────────────────────────────────

fn path_to_root(node: Arc<CgroupNode>) -> Vec<Arc<CgroupNode>> {
    let mut path = Vec::new();
    let mut current = Some(node);
    while let Some(n) = current {
        current = n.parent.as_ref().and_then(|p| p.upgrade());
        path.push(n);
    }
    path
}

fn charge_path(path: &[Arc<CgroupNode>]) -> VfsResult<()> {
    for (charged, node) in path.iter().enumerate() {
        if let Err(err) = node.pids.try_charge_local() {
            for n in &path[..charged] {
                n.pids.uncharge_local();
            }
            return Err(err);
        }
    }
    Ok(())
}

fn uncharge_path(path: &[Arc<CgroupNode>]) {
    for node in path {
        node.pids.uncharge_local();
    }
}

fn is_domain_controller(name: &str) -> bool {
    controller::get_factory(name).is_some_and(|f| f.is_domain())
}

fn has_domain_subtree_control(node: &CgroupNode) -> bool {
    node.subtree_control
        .lock()
        .iter()
        .any(|c| is_domain_controller(c))
}

fn can_host_process(node: &CgroupNode) -> bool {
    node.id == root_id() || !has_domain_subtree_control(node)
}

fn pending_count_in_node(membership: &MembershipState, id: CgroupId) -> usize {
    membership
        .pending_pids
        .values()
        .filter(|&&pid_id| pid_id == id)
        .count()
}

fn node_has_processes_or_pending(membership: &MembershipState, node: &CgroupNode) -> bool {
    !node.procs.lock().is_empty() || pending_count_in_node(membership, node.id) != 0
}

fn add_process_to_node(node: &CgroupNode, pid: u32) {
    let mut procs = node.procs.lock();
    if !procs.contains(&pid) {
        procs.push(pid);
    }
}

fn remove_process_from_node(node: &CgroupNode, pid: u32) -> bool {
    let mut procs = node.procs.lock();
    let old_len = procs.len();
    procs.retain(|&m| m != pid);
    procs.len() != old_len
}

fn controller_available(node: &CgroupNode, name: &str) -> bool {
    if node.id == root_id() {
        return node.controllers.contains_key(name);
    }
    node.parent
        .as_ref()
        .and_then(|p| p.upgrade())
        .is_some_and(|parent| parent.subtree_control.lock().iter().any(|c| c == name))
}

pub fn attach_initial_process(pid: u32) -> VfsResult<()> {
    let mut membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();
    let root = core::get_node(root_id())?;
    if !root.procs.lock().contains(&pid) {
        charge_path(&path_to_root(root.clone()))?;
        add_process_to_node(&root, pid);
    }
    membership.detached_pids.remove(&pid);
    Ok(())
}

pub struct CgroupForkGuard {
    cgroup: Arc<CgroupNode>,
    charged_path: Vec<Arc<CgroupNode>>,
    pid: u32,
    committed: bool,
}

impl CgroupForkGuard {
    pub fn commit(mut self) {
        let mut membership = MEMBERSHIP
            .get()
            .expect("cgroup membership initialized")
            .lock();
        membership.pending_pids.remove(&self.pid);
        membership.detached_pids.remove(&self.pid);
        add_process_to_node(&self.cgroup, self.pid);
        self.committed = true;
    }
}

impl Drop for CgroupForkGuard {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(membership) = MEMBERSHIP.get() {
                membership.lock().pending_pids.remove(&self.pid);
            }
            uncharge_path(&self.charged_path);
        }
    }
}

pub fn begin_fork(parent_cgroup: &Arc<CgroupNode>, pid: u32) -> VfsResult<CgroupForkGuard> {
    let mut membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();
    if !can_host_process(parent_cgroup) {
        return Err(VfsError::ResourceBusy);
    }
    if membership.pending_pids.contains_key(&pid) {
        return Err(VfsError::ResourceBusy);
    }
    let charged_path = path_to_root(parent_cgroup.clone());
    charge_path(&charged_path)?;
    membership.pending_pids.insert(pid, parent_cgroup.id);
    Ok(CgroupForkGuard {
        cgroup: parent_cgroup.clone(),
        charged_path,
        pid,
        committed: false,
    })
}

pub fn migrate_process(pid: u32, target_id: CgroupId) -> VfsResult<()> {
    let mut membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();
    let target = core::get_node(target_id)?;
    if !can_host_process(&target) {
        return Err(VfsError::ResourceBusy);
    }
    if membership.pending_pids.contains_key(&pid) {
        return Err(VfsError::ResourceBusy);
    }

    with_provider(|provider| {
        if membership.detached_pids.contains(&pid) || provider.is_zombie(pid) {
            return Err(VfsError::NoSuchProcess);
        }

        let old = provider.get_cgroup(pid).ok_or(VfsError::NotFound)?;
        if old.id == target.id {
            return Ok(());
        }
        if !old.procs.lock().contains(&pid) {
            return Err(VfsError::NoSuchProcess);
        }

        let target_path = path_to_root(target.clone());
        let old_path = path_to_root(old.clone());
        let mut target_unique_len = target_path.len();
        let mut old_unique_len = old_path.len();
        while target_unique_len > 0
            && old_unique_len > 0
            && target_path[target_unique_len - 1].id == old_path[old_unique_len - 1].id
        {
            target_unique_len -= 1;
            old_unique_len -= 1;
        }

        charge_path(&target_path[..target_unique_len])?;

        if !remove_process_from_node(&old, pid) {
            uncharge_path(&target_path[..target_unique_len]);
            return Err(VfsError::NoSuchProcess);
        }
        add_process_to_node(&target, pid);
        provider.set_cgroup(pid, target);
        membership.detached_pids.remove(&pid);
        uncharge_path(&old_path[..old_unique_len]);
        Ok(())
    })
}

pub fn exit_process(pid: u32) -> VfsResult<()> {
    let mut membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();

    with_provider(|provider| {
        let cgroup = provider.get_cgroup(pid).ok_or(VfsError::NotFound)?;
        if remove_process_from_node(&cgroup, pid) {
            uncharge_path(&path_to_root(cgroup));
        }
        membership.detached_pids.insert(pid);
        Ok(())
    })
}
