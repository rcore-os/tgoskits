//! cgroup v2 subsystem for StarryOS.
//!
//! This crate provides the core cgroup v2 hierarchy, controller state,
//! and membership management. It is kernel-independent — the kernel
//! provides a [`CgroupProvider`] implementation to supply task/process
//! primitives.

#![no_std]

extern crate alloc;

mod core;
pub mod cpu;
pub mod pids;
pub mod provider;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
pub use core::{CgroupNode, GLOBAL_CGROUP_ROOT};

use ::core::{fmt::Write, str, sync::atomic::Ordering};
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

struct AttrInfo {
    name: &'static str,
    read_only: bool,
}

const CONTROLLER_ATTRS: &[AttrInfo] = &[
    AttrInfo {
        name: "pids.max",
        read_only: false,
    },
    AttrInfo {
        name: "pids.current",
        read_only: true,
    },
    AttrInfo {
        name: "cpu.weight",
        read_only: false,
    },
    AttrInfo {
        name: "cpu.max",
        read_only: false,
    },
    AttrInfo {
        name: "cpu.stat",
        read_only: true,
    },
];

struct MembershipState {
    detached_pids: BTreeSet<u32>,
    pending_pids: BTreeMap<u32, CgroupId>,
}

static MEMBERSHIP: LazyInit<SpinNoIrq<MembershipState>> = LazyInit::new();

static PROVIDER: LazyInit<provider::ProviderCell> = LazyInit::new();

/// Initialize the cgroup subsystem. Called once during boot.
pub fn init() {
    MEMBERSHIP.init_once(SpinNoIrq::new(MembershipState {
        detached_pids: BTreeSet::new(),
        pending_pids: BTreeMap::new(),
    }));
    core::init();
    PROVIDER.init_once(provider::ProviderCell::new());
    info!("cgroup: initialized");
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
            if !next.iter().any(|controller| controller == name) {
                next.push(name.to_string());
            }
        } else if let Some(name) = part.strip_prefix('-') {
            if !controller_available(&node, name) {
                return Err(VfsError::InvalidInput);
            }
            next.retain(|controller| controller != name);
        } else {
            return Err(VfsError::InvalidInput);
        }
    }
    next.sort_by_key(|controller| match controller.as_str() {
        "pids" => 0,
        "cpu" => 1,
        _ => 2,
    });
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

fn path_to_root(node: Arc<CgroupNode>) -> Vec<Arc<CgroupNode>> {
    let mut path = Vec::new();
    let mut current = Some(node);
    while let Some(node) = current {
        current = node.parent.as_ref().and_then(|parent| parent.upgrade());
        path.push(node);
    }
    path
}

fn charge_path(path: &[Arc<CgroupNode>]) -> VfsResult<()> {
    for (charged, node) in path.iter().enumerate() {
        if let Err(err) = node.pids.try_charge_local() {
            for charged_node in &path[..charged] {
                charged_node.pids.uncharge_local();
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
    name == "cpu"
}

fn has_domain_subtree_control(node: &CgroupNode) -> bool {
    node.subtree_control
        .lock()
        .iter()
        .any(|controller| is_domain_controller(controller))
}

fn can_host_process(node: &CgroupNode) -> bool {
    node.id == root_id() || !has_domain_subtree_control(node)
}

fn pending_count_in_node(membership: &MembershipState, id: CgroupId) -> usize {
    membership
        .pending_pids
        .values()
        .filter(|&&pending_id| pending_id == id)
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
    procs.retain(|&member| member != pid);
    procs.len() != old_len
}

pub fn attach_initial_process(pid: u32) -> VfsResult<()> {
    let mut membership = MEMBERSHIP.get().ok_or(VfsError::BadState)?.lock();
    let root = core::get_node(root_id())?;
    charge_path(&path_to_root(root.clone()))?;
    add_process_to_node(&root, pid);
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
                let mut membership = membership.lock();
                membership.pending_pids.remove(&self.pid);
                uncharge_path(&self.charged_path);
            } else {
                uncharge_path(&self.charged_path);
            }
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

pub fn all_attr_names(id: CgroupId) -> VfsResult<Vec<String>> {
    let node = core::get_node(id)?;
    Ok(CONTROLLER_ATTRS
        .iter()
        .filter(|attr| attr_available(&node, attr.name))
        .map(|attr| attr.name.to_string())
        .collect())
}

pub fn is_controller_attr(id: CgroupId, name: &str) -> VfsResult<bool> {
    let node = core::get_node(id)?;
    Ok(CONTROLLER_ATTRS
        .iter()
        .any(|attr| attr.name == name && attr_available(&node, name)))
}

pub fn attr_is_read_only(id: CgroupId, name: &str) -> VfsResult<Option<bool>> {
    ensure_node_exists(id)?;
    Ok(CONTROLLER_ATTRS
        .iter()
        .find(|attr| attr.name == name)
        .map(|attr| attr.read_only))
}

pub fn is_interface_file_name(name: &str) -> bool {
    BUILTIN_FILES.contains(&name) || CONTROLLER_ATTRS.iter().any(|attr| attr.name == name)
}

fn attr_owner(name: &str) -> Option<&str> {
    name.split_once('.').map(|(owner, _)| owner)
}

fn controller_available(node: &CgroupNode, name: &str) -> bool {
    if node.id == root_id() {
        return node.controllers.iter().any(|controller| controller == name);
    }
    node.parent
        .as_ref()
        .and_then(|parent| parent.upgrade())
        .is_some_and(|parent| {
            parent
                .subtree_control
                .lock()
                .iter()
                .any(|controller| controller == name)
        })
}

fn attr_available(node: &CgroupNode, name: &str) -> bool {
    let Some(owner) = attr_owner(name) else {
        return false;
    };
    controller_available(node, owner)
}

pub fn read_attr_at(id: CgroupId, name: &str, offset: usize, buf: &mut [u8]) -> VfsResult<usize> {
    if !is_controller_attr(id, name)? {
        return Err(VfsError::NotFound);
    }
    let value = match name {
        "pids.max" => {
            let max = core::get_node(id)?.pids.max.load(Ordering::Acquire);
            if max < 0 {
                "max\n".to_string()
            } else {
                format!("{}\n", max)
            }
        }
        "pids.current" => format!(
            "{}\n",
            core::get_node(id)?.pids.current.load(Ordering::Acquire)
        ),
        "cpu.weight" => format!(
            "{}\n",
            core::get_node(id)?.cpu.weight.load(Ordering::Acquire)
        ),
        "cpu.max" => {
            let node = core::get_node(id)?;
            let quota = node.cpu.cfs_quota.load(Ordering::Acquire);
            let period = node.cpu.cfs_period.load(Ordering::Acquire);
            if quota < 0 {
                format!("max {}\n", period)
            } else {
                format!("{} {}\n", quota, period)
            }
        }
        "cpu.stat" => {
            let node = core::get_node(id)?;
            let bw = &node.cpu.bandwidth;
            format!(
                "nr_periods {}\nnr_throttled {}\nthrottled_usec {}\n",
                bw.nr_periods.load(Ordering::Acquire),
                bw.nr_throttled.load(Ordering::Acquire),
                bw.throttled_usec.load(Ordering::Acquire),
            )
        }
        _ => return Err(VfsError::NotFound),
    };

    let bytes = value.as_bytes();
    if offset >= bytes.len() {
        return Ok(0);
    }
    let remaining = &bytes[offset..];
    let n = remaining.len().min(buf.len());
    buf[..n].copy_from_slice(&remaining[..n]);
    Ok(n)
}

pub fn write_attr(id: CgroupId, name: &str, data: &[u8]) -> VfsResult<usize> {
    let node = core::get_node(id)?;
    if !is_controller_attr(id, name)? {
        return Err(VfsError::NotFound);
    }
    let text = str::from_utf8(data)
        .map_err(|_| VfsError::InvalidInput)?
        .trim();
    match name {
        "pids.max" => {
            let value = if text == "max" {
                -1
            } else {
                text.parse::<i64>().map_err(|_| VfsError::InvalidInput)?
            };
            if text != "max" && value < 0 {
                return Err(VfsError::InvalidInput);
            }
            node.pids.max.store(value, Ordering::Release);
        }
        "pids.current" | "cpu.stat" => return Err(VfsError::OperationNotPermitted),
        "cpu.weight" => {
            let value = text.parse::<i64>().map_err(|_| VfsError::InvalidInput)?;
            if !(1..=10_000).contains(&value) {
                return Err(VfsError::InvalidInput);
            }
            node.cpu.weight.store(value, Ordering::Release);
        }
        "cpu.max" => write_cpu_max(&node, text)?,
        _ => return Err(VfsError::NotFound),
    }
    Ok(data.len())
}

fn write_cpu_max(node: &CgroupNode, text: &str) -> VfsResult<()> {
    let parts = text.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 2 {
        return Err(VfsError::InvalidInput);
    }
    let quota = if parts[0] == "max" {
        -1
    } else {
        let quota = parts[0]
            .parse::<i64>()
            .map_err(|_| VfsError::InvalidInput)?;
        if quota <= 0 {
            return Err(VfsError::InvalidInput);
        }
        quota
    };
    let period = if parts.len() == 2 {
        let period = parts[1]
            .parse::<i64>()
            .map_err(|_| VfsError::InvalidInput)?;
        if !(1_000..=1_000_000).contains(&period) {
            return Err(VfsError::InvalidInput);
        }
        period
    } else {
        node.cpu.cfs_period.load(Ordering::Acquire)
    };

    node.cpu.cfs_quota.store(quota, Ordering::Release);
    node.cpu.cfs_period.store(period, Ordering::Release);
    node.cpu.bandwidth.quota.store(quota, Ordering::Release);
    node.cpu.bandwidth.period.store(period, Ordering::Release);
    node.cpu.bandwidth.consumed.store(0, Ordering::Release);
    node.cpu.bandwidth.period_start.store(0, Ordering::Release);
    Ok(())
}
