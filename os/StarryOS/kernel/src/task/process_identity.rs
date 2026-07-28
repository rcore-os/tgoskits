//! Stable Linux PID identity and lifecycle transitions.
//!
//! A numeric PID is only a registry key. [`ProcessIdentity`] is the
//! generation-specific object retained by pidfds from live publication through
//! zombie observation and final reap.

use alloc::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};

use ax_errno::{AxError, AxResult};
use ax_kspin::{SpinNoIrq, SpinRwLock as RwLock};
use axpoll::{IoEvents, PollSet};
use starry_process::{Pid, Process, ProcessCpuTime, init_proc};

use super::{Cred, ProcessData, current_user_task};

/// Generation-specific identity retained by the PID registry and pidfds.
pub(crate) struct ProcessIdentity {
    process: Arc<Process>,
    pid_namespaces: Arc<[axnsproxy::PidNamespaceRef]>,
    exit_event: Arc<PollSet>,
    state: SpinNoIrq<ProcessIdentityState>,
}

enum ProcessIdentityState {
    Live(Weak<ProcessData>),
    Zombie(ZombieSnapshot),
    Reaping,
    Reaped,
}

impl ProcessIdentityState {
    fn is_publicly_resolvable(&self) -> bool {
        matches!(self, Self::Live(_) | Self::Zombie(_))
    }
}

/// Immutable process-exit data retained until one consuming wait reaps it.
pub(crate) struct ZombieSnapshot {
    pub(crate) cred: Arc<Cred>,
    pub(crate) nice: i32,
    pub(crate) ptrace_tracer_pid: Option<Pid>,
    pub(crate) is_clone_child: bool,
    pub(crate) wait_parent_tid: Pid,
    pub(crate) cpu_time: ProcessCpuTime,
}

impl ProcessIdentity {
    pub(super) fn new(
        process: Arc<Process>,
        exit_event: Arc<PollSet>,
        proc_data: Weak<ProcessData>,
        pid_namespaces: Arc<[axnsproxy::PidNamespaceRef]>,
    ) -> Arc<Self> {
        Arc::new(Self {
            process,
            pid_namespaces,
            exit_event,
            state: SpinNoIrq::new(ProcessIdentityState::Live(proc_data)),
        })
    }

    /// Returns the stable process object for this PID generation.
    pub(crate) fn process(&self) -> Arc<Process> {
        self.process.clone()
    }

    /// Returns the numeric PID lookup key.
    pub(crate) fn pid(&self) -> Pid {
        self.process.pid()
    }

    /// Returns the immutable PID namespace membership for this generation.
    pub(crate) fn pid_namespace(&self) -> axnsproxy::PidNamespaceRef {
        self.pid_namespaces[0].clone()
    }

    /// Returns all PID namespace identities from the active level to root.
    pub(crate) fn pid_namespaces(&self) -> &[axnsproxy::PidNamespaceRef] {
        &self.pid_namespaces
    }

    /// Returns whether this generation is visible in one namespace level.
    pub(crate) fn belongs_to_pid_namespace(&self, namespace: &axnsproxy::PidNamespaceRef) -> bool {
        self.pid_namespaces
            .iter()
            .any(|member| Arc::ptr_eq(member, namespace))
    }

    /// Returns the event shared by process pidfds across all lifecycle states.
    pub(crate) fn exit_event(&self) -> Arc<PollSet> {
        self.exit_event.clone()
    }

    /// Upgrades live runtime resources for operations that require them.
    pub(crate) fn live_data(&self) -> Option<Arc<ProcessData>> {
        let ProcessIdentityState::Live(proc_data) = &*self.state.lock() else {
            return None;
        };
        proc_data.upgrade()
    }

    /// Returns whether final process exit has been published but not consumed.
    pub(crate) fn is_zombie(&self) -> bool {
        matches!(*self.state.lock(), ProcessIdentityState::Zombie(_))
    }

    /// Returns whether one waiter consumed and retired this identity.
    pub(crate) fn is_reaped(&self) -> bool {
        matches!(
            *self.state.lock(),
            ProcessIdentityState::Reaping | ProcessIdentityState::Reaped
        )
    }

    /// Returns whether public PID lookup may resolve this identity.
    fn is_publicly_resolvable(&self) -> bool {
        self.state.lock().is_publicly_resolvable()
    }

    /// Resolves the process while this generation remains publicly visible.
    pub(crate) fn public_process(&self) -> AxResult<Arc<Process>> {
        let state = self.state.lock();
        if state.is_publicly_resolvable() {
            Ok(self.process.clone())
        } else {
            Err(AxError::NoSuchProcess)
        }
    }

    /// Returns process-pidfd readiness derived from the canonical lifecycle.
    pub(crate) fn poll_events(&self) -> IoEvents {
        match &*self.state.lock() {
            ProcessIdentityState::Live(_) => IoEvents::empty(),
            ProcessIdentityState::Zombie(_) => IoEvents::IN | IoEvents::RDNORM,
            ProcessIdentityState::Reaping | ProcessIdentityState::Reaped => {
                IoEvents::IN | IoEvents::RDNORM | IoEvents::HUP
            }
        }
    }

    pub(crate) fn matches_process(&self, process: &Process) -> bool {
        core::ptr::eq(self.process.as_ref(), process)
    }

    fn publish_zombie(
        &self,
        expected: &Arc<ProcessData>,
        zombie: ZombieSnapshot,
    ) -> Result<(), ZombieSnapshot> {
        let mut state = self.state.lock();
        if !Self::state_matches_live_data(&state, expected) {
            return Err(zombie);
        }
        *state = ProcessIdentityState::Zombie(zombie);
        Ok(())
    }

    fn matches_live_data(&self, expected: &Arc<ProcessData>) -> bool {
        Self::state_matches_live_data(&self.state.lock(), expected)
    }

    fn state_matches_live_data(state: &ProcessIdentityState, expected: &Arc<ProcessData>) -> bool {
        matches!(
            state,
            ProcessIdentityState::Live(proc_data)
                if proc_data
                    .upgrade()
                    .is_some_and(|registered| Arc::ptr_eq(&registered, expected))
        )
    }

    fn claim_reap(&self, expected: &Arc<Process>) -> Option<ZombieSnapshot> {
        if !self.matches_process(expected) {
            return None;
        }

        let mut state = self.state.lock();
        let ProcessIdentityState::Zombie(_) = &*state else {
            return None;
        };
        let ProcessIdentityState::Zombie(zombie) =
            core::mem::replace(&mut *state, ProcessIdentityState::Reaping)
        else {
            unreachable!("process identity changed while state-locked");
        };
        Some(zombie)
    }

    fn finish_reap(&self) {
        let mut state = self.state.lock();
        assert!(
            matches!(*state, ProcessIdentityState::Reaping),
            "only a uniquely claimed zombie can finish reaping"
        );
        *state = ProcessIdentityState::Reaped;
    }

    fn zombie_snapshot<R>(&self, f: impl FnOnce(&ZombieSnapshot) -> R) -> Option<R> {
        let state = self.state.lock();
        let ProcessIdentityState::Zombie(zombie) = &*state else {
            return None;
        };
        Some(f(zombie))
    }
}

static PROCESS_TABLE: RwLock<BTreeMap<Pid, Arc<ProcessIdentity>>> = RwLock::new(BTreeMap::new());

/// Registers the process identity associated with a newly published task.
pub(crate) fn register_process_identity(proc_data: &Arc<ProcessData>) {
    let pid = proc_data.proc.pid();
    let identity = proc_data.identity();
    let mut process_table = PROCESS_TABLE.write();
    match process_table.get(&pid) {
        Some(registered) if Arc::ptr_eq(registered, &identity) => {}
        Some(_) => panic!("PID must not be reused before its identity is reaped"),
        None => {
            process_table.insert(pid, identity);
        }
    }
}

/// Registers a freshly prepared process without replacing an existing PID
/// generation.
///
/// Unlike [`register_process_identity`], this reports a collision so clone can
/// roll back all resources before the scheduler thread becomes runnable.
pub(crate) fn register_prepared_process_identity(proc_data: &Arc<ProcessData>) -> AxResult<()> {
    let pid = proc_data.proc.pid();
    let identity = proc_data.identity();
    let mut process_table = PROCESS_TABLE.write();
    if process_table.contains_key(&pid) {
        return Err(AxError::BadState);
    }
    process_table.insert(pid, identity);
    Ok(())
}

/// Removes only the still-live identity installed for a failed prepared clone.
pub(crate) fn unregister_prepared_process_identity(proc_data: &Arc<ProcessData>) {
    let pid = proc_data.proc.pid();
    let identity = proc_data.identity();
    let mut process_table = PROCESS_TABLE.write();
    let matches = process_table.get(&pid).is_some_and(|registered| {
        Arc::ptr_eq(registered, &identity) && registered.matches_live_data(proc_data)
    });
    if matches {
        process_table.remove(&pid);
    }
}

/// Lists live process runtime resources.
pub fn processes() -> Vec<Arc<ProcessData>> {
    PROCESS_TABLE
        .read()
        .values()
        .filter_map(|identity| identity.live_data())
        .collect()
}

/// Finds live process runtime resources by PID.
pub fn get_process_data(pid: Pid) -> AxResult<Arc<ProcessData>> {
    if pid == 0 {
        return Ok(current_user_task().as_thread().proc_data.clone());
    }
    PROCESS_TABLE
        .read()
        .get(&pid)
        .and_then(|identity| identity.live_data())
        .ok_or(AxError::NoSuchProcess)
}

/// Resolves one stable generation for `pidfd_open()`.
pub(crate) fn pidfd_process_identity(pid: Pid) -> AxResult<Arc<ProcessIdentity>> {
    // Holding the registry read lock through the state check linearizes this
    // lookup against the write-locked Zombie -> Reaping claim.
    let process_table = PROCESS_TABLE.read();
    process_table
        .get(&pid)
        .filter(|identity| identity.is_publicly_resolvable())
        .cloned()
        .ok_or(AxError::NoSuchProcess)
}

/// Resolves the exact openable identity for a process object.
pub(crate) fn pidfd_thread_identity(process: &Arc<Process>) -> Option<Arc<ProcessIdentity>> {
    let process_table = PROCESS_TABLE.read();
    process_table
        .get(&process.pid())
        .filter(|identity| identity.matches_process(process))
        .filter(|identity| identity.is_publicly_resolvable())
        .cloned()
}

/// Resolves the exact registered identity for lifecycle observation.
fn process_identity(process: &Arc<Process>) -> Option<Arc<ProcessIdentity>> {
    PROCESS_TABLE
        .read()
        .get(&process.pid())
        .filter(|identity| identity.matches_process(process))
        .cloned()
}

/// Atomically replaces live runtime resources with an immutable zombie.
pub(crate) fn publish_zombie(proc_data: &Arc<ProcessData>, zombie: ZombieSnapshot) -> AxResult<()> {
    let process_table = PROCESS_TABLE.write();
    let Some(identity) = process_table.get(&proc_data.proc.pid()) else {
        return Err(AxError::BadState);
    };
    identity
        .publish_zombie(proc_data, zombie)
        .map_err(|_| AxError::BadState)
}

/// Reaps exactly one matching zombie and returns its frozen CPU time.
pub(crate) fn reap_process(process: &Arc<Process>) -> Option<ProcessCpuTime> {
    let (identity, zombie) = {
        let process_table = PROCESS_TABLE.write();
        let identity = process_table.get(&process.pid())?.clone();
        let zombie = identity.claim_reap(process)?;
        (identity, zombie)
    };

    #[cfg(axtest)]
    axtest::reap_claim_barrier(process.pid());

    // Keep the identity registered in Reaping while topology links are
    // removed. This prevents PID reuse from inserting a new process under the
    // same parent/group key before the old generation has retired.
    process.retire();
    {
        let mut process_table = PROCESS_TABLE.write();
        let registered = process_table
            .get(&process.pid())
            .expect("claimed identity must remain registered until reap finishes");
        assert!(
            Arc::ptr_eq(registered, &identity),
            "PID generation changed during reap"
        );
        identity.finish_reap();
        process_table.remove(&process.pid());
    }
    super::pid_namespace::release_process_pid(&identity);
    unsafe {
        identity
            .exit_event
            .wake(IoEvents::IN | IoEvents::RDNORM | IoEvents::HUP);
    }
    Some(zombie.cpu_time)
}

/// Returns whether `pid` names an exited, unreaped process.
pub fn is_zombie_pid(pid: Pid) -> bool {
    PROCESS_TABLE
        .read()
        .get(&pid)
        .is_some_and(|identity| identity.is_zombie())
}

/// Returns whether this exact process object is an exited, unreaped identity.
pub(crate) fn is_zombie_process(process: &Arc<Process>) -> bool {
    process_identity(process).is_some_and(|identity| identity.is_zombie())
}

/// Returns whether this exact process object has been reaped or superseded.
pub(crate) fn is_reaped_process(process: &Arc<Process>) -> bool {
    process_identity(process).is_none_or(|identity| identity.is_reaped())
}

fn is_live_process(process: &Arc<Process>) -> bool {
    process_identity(process).is_some_and(|identity| identity.live_data().is_some())
}

/// Relationship action selected for the last exiting thread of a process.
pub(crate) enum OrphanReaper {
    /// Reparent existing children to a live reaper in the same PID namespace.
    ReparentTo(Arc<Process>),
    /// Retain children while the PID namespace reaper shuts the namespace down.
    ShutdownNamespace(axnsproxy::PidNamespaceRef),
}

fn same_pid_namespace(process: &Arc<Process>, namespace: &axnsproxy::PidNamespaceRef) -> bool {
    process_identity(process)
        .is_some_and(|identity| Arc::ptr_eq(&identity.pid_namespace(), namespace))
}

fn registered_process(pid: Pid) -> Option<Arc<Process>> {
    PROCESS_TABLE
        .read()
        .get(&pid)
        .map(|identity| identity.process())
}

/// Returns the live namespace init that must autoreap this shutdown victim.
pub(crate) fn namespace_shutdown_parent(process: &Arc<Process>) -> Option<Arc<ProcessData>> {
    let identity = process_identity(process)?;
    let parent = process.parent()?;
    for namespace in identity.pid_namespaces() {
        if !namespace.is_shutting_down() {
            continue;
        }
        let init_pid = Pid::try_from(namespace.init_global_tid()?).ok()?;
        if parent.pid() != init_pid {
            continue;
        }
        let init_identity = PROCESS_TABLE.read().get(&init_pid)?.clone();
        if Arc::ptr_eq(&init_identity.process(), &parent)
            && init_identity.belongs_to_pid_namespace(namespace)
        {
            return init_identity.live_data();
        }
    }
    None
}

/// Returns whether a stable process generation belongs to this PID namespace.
pub(crate) fn process_belongs_to_pid_namespace(
    process: &Arc<Process>,
    namespace: &axnsproxy::PidNamespaceRef,
) -> bool {
    process_identity(process).is_some_and(|identity| identity.belongs_to_pid_namespace(namespace))
}

/// Chooses the nearest live child subreaper without crossing a PID namespace.
pub(crate) fn orphan_reaper_for(proc_data: &Arc<ProcessData>) -> OrphanReaper {
    let process = &proc_data.proc;
    let namespace = proc_data.identity().pid_namespace();
    let level = namespace.level();
    let init_global_tid = namespace.init_global_tid();

    if level > 0 && init_global_tid == Some(process.pid() as u64) {
        return OrphanReaper::ShutdownNamespace(namespace);
    }

    let init = if level == 0 {
        init_proc()
    } else {
        let init_pid = init_global_tid
            .and_then(|tid| Pid::try_from(tid).ok())
            .expect("published PID namespace must retain its init identity");
        registered_process(init_pid)
            .expect("PID namespace init identity must outlive every namespace member")
    };
    let mut cursor = process.parent();

    while let Some(candidate) = cursor {
        if Arc::ptr_eq(&candidate, &init) {
            break;
        }
        if !same_pid_namespace(&candidate, &namespace) {
            break;
        }
        if candidate.is_child_subreaper()
            && candidate.accepts_child_publication()
            && is_live_process(&candidate)
        {
            return OrphanReaper::ReparentTo(candidate);
        }
        cursor = candidate.parent();
    }
    OrphanReaper::ReparentTo(init)
}

/// Finds the stable process object for a publicly visible live or zombie PID.
pub fn get_process(pid: Pid) -> AxResult<Arc<Process>> {
    if pid == 0 {
        return Ok(current_user_task().as_thread().proc_data.proc.clone());
    }
    // Holding the registry read lock through the lifecycle check linearizes
    // lookup against the write-locked Zombie -> Reaping claim.
    let process_table = PROCESS_TABLE.read();
    process_table
        .get(&pid)
        .ok_or(AxError::NoSuchProcess)?
        .public_process()
}

/// Returns the credential snapshot for a zombie PID.
pub fn get_zombie_cred(pid: Pid) -> Option<Arc<Cred>> {
    PROCESS_TABLE
        .read()
        .get(&pid)?
        .zombie_snapshot(|zombie| zombie.cred.clone())
}

/// Returns the thread-group leader's nice value retained for a zombie PID.
pub fn get_zombie_nice(pid: Pid) -> Option<i32> {
    PROCESS_TABLE
        .read()
        .get(&pid)?
        .zombie_snapshot(|zombie| zombie.nice)
}

pub(crate) fn is_zombie_clone_child(pid: Pid) -> Option<bool> {
    PROCESS_TABLE
        .read()
        .get(&pid)?
        .zombie_snapshot(|zombie| zombie.is_clone_child)
}

pub(crate) fn zombie_wait_parent_tid(pid: Pid) -> Option<Pid> {
    PROCESS_TABLE
        .read()
        .get(&pid)?
        .zombie_snapshot(|zombie| zombie.wait_parent_tid)
}

pub(crate) fn traced_zombies_for(tracer_pid: Pid) -> Vec<Arc<Process>> {
    PROCESS_TABLE
        .read()
        .values()
        .filter(|identity| {
            identity
                .zombie_snapshot(|zombie| zombie.ptrace_tracer_pid == Some(tracer_pid))
                .is_some_and(|matches| matches)
        })
        .map(|identity| identity.process())
        .collect()
}

#[cfg(axtest)]
#[path = "process_identity_axtest.rs"]
mod axtest;

#[cfg(axtest)]
pub(crate) use axtest::reaping_identity_is_not_publicly_resolvable_for_test;
