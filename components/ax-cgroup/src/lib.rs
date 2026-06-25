//! cgroup v2 subsystem for StarryOS.
//!
//! This crate provides the core cgroup v2 hierarchy, controller state,
//! and membership management. It is kernel-independent — the kernel
//! provides a [`CgroupProvider`] implementation to supply task/process
//! primitives.

#![no_std]

extern crate alloc;
// Host-side unit tests need std (e.g. std::sync::Once); the non-test build
// stays strictly no_std.
#[cfg(test)]
extern crate std;

/// cgroup-specific error type, independent of any VFS implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CgroupError {
    /// Subsystem not initialized or provider not registered.
    NotInitialized,
    /// Requested cgroup node not found.
    NotFound,
    /// A cgroup with this name already exists.
    AlreadyExists,
    /// cgroup has active members, pending forks, or domain constraint violation.
    ResourceBusy,
    /// Invalid input (malformed UTF-8, out-of-range value, etc.).
    InvalidInput,
    /// Target process does not exist or is zombie.
    NoSuchProcess,
    /// Operation not permitted (e.g. writing read-only attribute).
    OperationNotPermitted,
    /// Cannot remove a cgroup that still has child cgroups.
    DirectoryNotEmpty,
    /// pids.max limit reached; Linux fork(2) semantics → EAGAIN.
    LimitExceeded,
}

pub type CgroupResult<T> = Result<T, CgroupError>;

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

fn with_provider<F, R>(f: F) -> CgroupResult<R>
where
    F: FnOnce(&dyn CgroupProvider) -> CgroupResult<R>,
{
    let cell = PROVIDER.get().ok_or(CgroupError::NotInitialized)?;
    let provider = cell.get().ok_or(CgroupError::NotInitialized)?;
    f(provider)
}

pub fn root_id() -> CgroupId {
    core::root_id()
}

pub fn path(id: CgroupId) -> CgroupResult<String> {
    core::path(id)
}

pub fn ensure_node_exists(id: CgroupId) -> CgroupResult<()> {
    core::get_node(id).map(|_| ())
}

pub fn child_names(id: CgroupId) -> CgroupResult<Vec<String>> {
    core::child_names(id)
}

pub fn lookup_child(parent_id: CgroupId, name: &str) -> CgroupResult<CgroupId> {
    core::lookup_child(parent_id, name)
}

pub fn create_child(parent_id: CgroupId, name: &str) -> CgroupResult<CgroupId> {
    let _membership = MEMBERSHIP.get().ok_or(CgroupError::NotInitialized)?.lock();
    if is_interface_file_name(name) || is_controller_attr(parent_id, name)? {
        return Err(CgroupError::AlreadyExists);
    }
    core::create_child(parent_id, name)
}

pub fn remove_child(parent_id: CgroupId, name: &str) -> CgroupResult<()> {
    let membership = MEMBERSHIP.get().ok_or(CgroupError::NotInitialized)?.lock();
    let child_id = core::lookup_child(parent_id, name)?;
    if pending_count_in_node(&membership, child_id) != 0 {
        return Err(CgroupError::ResourceBusy);
    }
    core::remove_child(parent_id, name)
}

pub fn controllers_text(id: CgroupId) -> CgroupResult<String> {
    Ok(core::get_node(id)?.controller_list())
}

pub fn procs_text(id: CgroupId) -> CgroupResult<String> {
    let node = core::get_node(id)?;
    let mut text = String::new();
    for pid in node.procs.lock().iter() {
        let _ = writeln!(text, "{}", pid);
    }
    Ok(text)
}

pub fn subtree_control_text(id: CgroupId) -> CgroupResult<String> {
    let node = core::get_node(id)?;
    Ok(node.subtree_control.lock().join(" "))
}

pub fn write_subtree_control(id: CgroupId, data: &[u8]) -> CgroupResult<()> {
    let membership = MEMBERSHIP.get().ok_or(CgroupError::NotInitialized)?.lock();
    let node = core::get_node(id)?;
    let text = str::from_utf8(data)
        .map_err(|_| CgroupError::InvalidInput)?
        .trim();
    let mut next = node.subtree_control.lock().clone();
    for part in text.split_whitespace() {
        if let Some(name) = part.strip_prefix('+') {
            if !controller_available(&node, name) {
                return Err(CgroupError::InvalidInput);
            }
            if is_domain_controller(name)
                && node.id != root_id()
                && node_has_processes_or_pending(&membership, &node)
            {
                return Err(CgroupError::ResourceBusy);
            }
            if !next.iter().any(|controller| controller == name) {
                next.push(name.to_string());
            }
        } else if let Some(name) = part.strip_prefix('-') {
            if !controller_available(&node, name) {
                return Err(CgroupError::InvalidInput);
            }
            next.retain(|controller| controller != name);
        } else {
            return Err(CgroupError::InvalidInput);
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

pub fn write_procs(id: CgroupId, data: &[u8]) -> CgroupResult<()> {
    let text = str::from_utf8(data)
        .map_err(|_| CgroupError::InvalidInput)?
        .trim();
    let pid: u32 = text.parse().map_err(|_| CgroupError::InvalidInput)?;
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

fn charge_path(path: &[Arc<CgroupNode>]) -> CgroupResult<()> {
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

pub fn attach_initial_process(pid: u32) -> CgroupResult<()> {
    let mut membership = MEMBERSHIP.get().ok_or(CgroupError::NotInitialized)?.lock();
    let root = core::get_node(root_id())?;
    charge_path(&path_to_root(root.clone()))?;
    add_process_to_node(&root, pid);
    membership.detached_pids.remove(&pid);
    Ok(())
}

/// Three-state lifecycle for CgroupForkGuard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardState {
    /// begin_fork succeeded; charge is held but membership not yet committed.
    Pending,
    /// commit() called; membership is finalized, pid is in procs list.
    Committed,
    /// cancel_fork() called after commit; membership reversed, Drop is no-op.
    Cancelled,
}

pub struct CgroupForkGuard {
    cgroup: Arc<CgroupNode>,
    charged_path: Vec<Arc<CgroupNode>>,
    pid: u32,
    state: GuardState,
}

impl ::core::fmt::Debug for CgroupForkGuard {
    fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
        // CgroupNode is intentionally not Debug (large, self-referential via
        // Weak parent links), so report the guard by its stable identity only.
        f.debug_struct("CgroupForkGuard")
            .field("cgroup", &self.cgroup.id)
            .field("pid", &self.pid)
            .field("state", &self.state)
            .finish()
    }
}

impl CgroupForkGuard {
    /// Finalize fork: register membership.
    /// Must be called BEFORE child becomes runnable (before spawn_task).
    ///
    /// Deviation from reviewer B3: uses `&mut self` instead of `self` to
    /// allow subsequent `cancel_fork()` call if spawn_task fails.
    pub fn commit(&mut self) {
        debug_assert!(
            self.state == GuardState::Pending,
            "commit called on non-Pending guard: {:?}",
            self.state
        );
        let mut membership = MEMBERSHIP
            .get()
            .expect("cgroup membership initialized")
            .lock();
        membership.pending_pids.remove(&self.pid);
        membership.detached_pids.remove(&self.pid);
        add_process_to_node(&self.cgroup, self.pid);
        self.state = GuardState::Committed;
    }

    /// Cancel a committed fork (e.g. if a future spawn_task fails after commit).
    /// Reverses all commit effects; after this call, Drop is a no-op.
    /// Linux equivalent: cgroup_cancel_fork().
    ///
    /// INVARIANT: Execution order is strict —
    ///   1. remove_process_from_node (reverse membership)
    ///   2. uncharge_path (release pids charge)
    ///   3. set state = Cancelled
    ///
    /// Reversing steps 1 and 2 would create a window where other CPUs see
    /// membership present but charge already decremented.
    ///
    /// NOTE: cancel_fork does NOT touch pending_pids or detached_pids —
    /// commit() already removed the pid from both maps. Re-removing here
    /// would be a no-op but misleading to future readers.
    ///
    /// Idempotent: calling cancel_fork on an already-Cancelled guard is a no-op.
    pub fn cancel_fork(&mut self) {
        if self.state == GuardState::Cancelled {
            return; // Already cancelled — idempotent.
        }
        // Hard state precondition: cancel_fork is ONLY meaningful for
        // Committed guards. Pending should never reach here (call site bug);
        // Cancelled is handled above. Fires in debug + test builds.
        debug_assert_eq!(
            self.state,
            GuardState::Committed,
            "cancel_fork: expected Committed, got {:?}",
            self.state
        );
        // Step 1: Reverse commit — remove from procs list.
        remove_process_from_node(&self.cgroup, self.pid);
        // Step 2: Uncharge the path immediately — do NOT rely on Drop.
        uncharge_path(&self.charged_path);
        // Step 3: Transition to terminal Cancelled state; Drop becomes no-op.
        self.state = GuardState::Cancelled;
    }
}

impl Drop for CgroupForkGuard {
    fn drop(&mut self) {
        match self.state {
            GuardState::Pending => {
                // Never committed: rollback pending + uncharge.
                // MEMBERSHIP cleanup is best-effort; if uninitialized (extreme
                // panic scenario), skip pending cleanup only.
                if let Some(membership) = MEMBERSHIP.get() {
                    let mut membership = membership.lock();
                    membership.pending_pids.remove(&self.pid);
                }
                // VERIFIED FACT: uncharge_path operates only on AtomicI64
                // in CgroupNode.pids (see uncharge_local impl); it does NOT
                // access MEMBERSHIP or any global state. charge_path and
                // path_to_root are also MEMBERSHIP-free.
                // Therefore, safe to call unconditionally even if MEMBERSHIP
                // was never initialized.
                uncharge_path(&self.charged_path);
            }
            GuardState::Committed | GuardState::Cancelled => {
                // Committed: membership finalized, child owns its charge.
                // Cancelled: cancel_fork already reversed everything.
                // Both are terminal states — Drop MUST be a no-op.
            }
        }
    }
}

pub fn begin_fork(parent_cgroup: &Arc<CgroupNode>, pid: u32) -> CgroupResult<CgroupForkGuard> {
    let mut membership = MEMBERSHIP.get().ok_or(CgroupError::NotInitialized)?.lock();
    if !can_host_process(parent_cgroup) {
        return Err(CgroupError::ResourceBusy);
    }
    if membership.pending_pids.contains_key(&pid) {
        return Err(CgroupError::ResourceBusy);
    }
    let charged_path = path_to_root(parent_cgroup.clone());
    charge_path(&charged_path)?;
    membership.pending_pids.insert(pid, parent_cgroup.id);
    Ok(CgroupForkGuard {
        cgroup: parent_cgroup.clone(),
        charged_path,
        pid,
        state: GuardState::Pending,
    })
}

pub fn migrate_process(pid: u32, target_id: CgroupId) -> CgroupResult<()> {
    let mut membership = MEMBERSHIP.get().ok_or(CgroupError::NotInitialized)?.lock();
    let target = core::get_node(target_id)?;
    if !can_host_process(&target) {
        return Err(CgroupError::ResourceBusy);
    }
    if membership.pending_pids.contains_key(&pid) {
        return Err(CgroupError::ResourceBusy);
    }

    with_provider(|provider| {
        if membership.detached_pids.contains(&pid) || provider.is_zombie(pid) {
            return Err(CgroupError::NoSuchProcess);
        }

        let old = provider.get_cgroup(pid).ok_or(CgroupError::NotFound)?;
        if old.id == target.id {
            return Ok(());
        }
        if !old.procs.lock().contains(&pid) {
            return Err(CgroupError::NoSuchProcess);
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
            return Err(CgroupError::NoSuchProcess);
        }
        add_process_to_node(&target, pid);
        provider.set_cgroup(pid, target);
        membership.detached_pids.remove(&pid);
        uncharge_path(&old_path[..old_unique_len]);
        Ok(())
    })
}

pub fn exit_process(pid: u32) -> CgroupResult<()> {
    let mut membership = MEMBERSHIP.get().ok_or(CgroupError::NotInitialized)?.lock();

    // INVARIANT: If commit is always before spawn_task, a pid in pending_pids
    // should never reach exit_process — the child is not yet runnable.
    // If this assert fires, there is a bug in the fork ordering.
    debug_assert!(
        !membership.pending_pids.contains_key(&pid),
        "exit_process called on pending pid {pid} — fork ordering violation"
    );

    with_provider(|provider| {
        let cgroup = provider.get_cgroup(pid).ok_or(CgroupError::NotFound)?;
        // remove_process_from_node returns false if pid was already removed
        // (idempotent — prevents double uncharge on repeated exit_process calls).
        if remove_process_from_node(&cgroup, pid) {
            uncharge_path(&path_to_root(cgroup));
        }
        membership.detached_pids.insert(pid);
        Ok(())
    })
}

pub fn all_attr_names(id: CgroupId) -> CgroupResult<Vec<String>> {
    let node = core::get_node(id)?;
    Ok(CONTROLLER_ATTRS
        .iter()
        .filter(|attr| attr_available(&node, attr.name))
        .map(|attr| attr.name.to_string())
        .collect())
}

pub fn is_controller_attr(id: CgroupId, name: &str) -> CgroupResult<bool> {
    let node = core::get_node(id)?;
    Ok(CONTROLLER_ATTRS
        .iter()
        .any(|attr| attr.name == name && attr_available(&node, name)))
}

pub fn attr_is_read_only(id: CgroupId, name: &str) -> CgroupResult<Option<bool>> {
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

pub fn read_attr_at(
    id: CgroupId,
    name: &str,
    offset: usize,
    buf: &mut [u8],
) -> CgroupResult<usize> {
    if !is_controller_attr(id, name)? {
        return Err(CgroupError::NotFound);
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
        _ => return Err(CgroupError::NotFound),
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

pub fn write_attr(id: CgroupId, name: &str, data: &[u8]) -> CgroupResult<usize> {
    let node = core::get_node(id)?;
    if !is_controller_attr(id, name)? {
        return Err(CgroupError::NotFound);
    }
    let text = str::from_utf8(data)
        .map_err(|_| CgroupError::InvalidInput)?
        .trim();
    match name {
        "pids.max" => {
            let value = if text == "max" {
                -1
            } else {
                text.parse::<i64>().map_err(|_| CgroupError::InvalidInput)?
            };
            if text != "max" && value < 0 {
                return Err(CgroupError::InvalidInput);
            }
            node.pids.max.store(value, Ordering::Release);
        }
        "pids.current" | "cpu.stat" => return Err(CgroupError::OperationNotPermitted),
        "cpu.weight" => {
            let value = text.parse::<i64>().map_err(|_| CgroupError::InvalidInput)?;
            if !(1..=10_000).contains(&value) {
                return Err(CgroupError::InvalidInput);
            }
            node.cpu.weight.store(value, Ordering::Release);
        }
        "cpu.max" => write_cpu_max(&node, text)?,
        _ => return Err(CgroupError::NotFound),
    }
    Ok(data.len())
}

fn write_cpu_max(node: &CgroupNode, text: &str) -> CgroupResult<()> {
    let parts = text.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 2 {
        return Err(CgroupError::InvalidInput);
    }
    let quota = if parts[0] == "max" {
        -1
    } else {
        let quota = parts[0]
            .parse::<i64>()
            .map_err(|_| CgroupError::InvalidInput)?;
        if quota <= 0 {
            return Err(CgroupError::InvalidInput);
        }
        quota
    };
    let period = if parts.len() == 2 {
        let period = parts[1]
            .parse::<i64>()
            .map_err(|_| CgroupError::InvalidInput)?;
        if !(1_000..=1_000_000).contains(&period) {
            return Err(CgroupError::InvalidInput);
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

#[cfg(test)]
mod tests {
    use super::*;

    struct MockProvider;
    impl CgroupProvider for MockProvider {
        fn is_zombie(&self, _pid: u32) -> bool {
            true
        }
        fn get_cgroup(&self, _pid: u32) -> Option<Arc<CgroupNode>> {
            None
        }
        fn set_cgroup(&self, _pid: u32, _cgroup: Arc<CgroupNode>) {}
    }

    static INIT: std::sync::Once = std::sync::Once::new();

    fn ensure_init() {
        INIT.call_once(|| {
            init();
            register_provider(&MockProvider);
        });
    }

    #[test]
    fn test_charge_uncharge_roundtrip() {
        ensure_init();
        let root = core::GLOBAL_CGROUP_ROOT.get().expect("root initialized");
        let before = root.pids.current.load(Ordering::Acquire);
        let path = path_to_root(root.clone());
        charge_path(&path).expect("charge should succeed");
        let after_charge = root.pids.current.load(Ordering::Acquire);
        assert_eq!(after_charge, before + 1);
        uncharge_path(&path);
        let after_uncharge = root.pids.current.load(Ordering::Acquire);
        assert_eq!(after_uncharge, before);
    }

    #[test]
    fn test_fork_limit_exceeded() {
        ensure_init();
        let root = core::GLOBAL_CGROUP_ROOT.get().expect("root initialized");
        // Set pids.max = 1
        root.pids.max.store(1, Ordering::Release);
        // First begin_fork should succeed (current=0 < max=1)
        let mut guard = begin_fork(&root, 10001).expect("first begin_fork");
        guard.commit();
        // Second begin_fork should fail (current=1 >= max=1)
        let result = begin_fork(&root, 10002);
        assert_eq!(result.unwrap_err(), CgroupError::LimitExceeded);
        // Cleanup: remove process, uncharge, restore pids.max
        remove_process_from_node(&root, 10001);
        uncharge_path(&path_to_root(root.clone()));
        root.pids.max.store(-1, Ordering::Release);
    }
}
