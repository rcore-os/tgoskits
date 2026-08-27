use alloc::{
    collections::btree_set::BTreeSet,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_lazyinit::LazyInit;
use weak_map::StrongMap;

use super::{ProcessGroup, Session};
use crate::{
    sync::SpinLock,
    task::{PidIdentity, TgidNumber, TidNumber},
};

const NESTED_CHILDREN_LOCK_SUBCLASS: u32 = 1;

#[derive(Default)]
pub(crate) struct ThreadGroup {
    pub(crate) threads: BTreeSet<TidNumber>,
    pub(crate) exit_code: i32,
    pub(crate) group_exited: bool,
    pub(crate) exited_cpu_time: ProcessCpuTime,
}

/// CPU time accumulated by threads that have exited from a process.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProcessCpuTime {
    user: Duration,
    system: Duration,
}

impl ProcessCpuTime {
    /// Creates a process CPU-time value.
    pub const fn new(user: Duration, system: Duration) -> Self {
        Self { user, system }
    }

    /// Returns time spent executing in user mode.
    pub const fn user(self) -> Duration {
        self.user
    }

    /// Returns time spent executing in kernel mode.
    pub const fn system(self) -> Duration {
        self.system
    }

    fn add(&mut self, other: Self) {
        self.user += other.user;
        self.system += other.system;
    }
}

/// Result of removing one TID from a process thread group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThreadExit {
    /// The TID had already left the thread group.
    AlreadyExited,
    /// Other threads remain alive.
    Remaining,
    /// This was the last thread; the payload is the frozen process CPU time.
    Last(ProcessCpuTime),
}

/// A process.
pub struct Process {
    pid: TgidNumber,
    identity: Weak<PidIdentity>,
    is_child_subreaper: AtomicBool,
    pub(crate) tg: SpinLock<ThreadGroup>,

    children: SpinLock<StrongMap<TgidNumber, Arc<Process>>>,
    parent: SpinLock<Weak<Process>>,

    /// Serializes job-control topology transitions for this process.
    ///
    /// Process-group membership locks are ordered by PGID, and the `group`
    /// pointer is changed only after both registries contain the new state.
    job_control: SpinLock<()>,
    group: SpinLock<Arc<ProcessGroup>>,
}

impl Process {
    /// The root-namespace thread-group ID of this process.
    pub const fn pid(&self) -> TgidNumber {
        self.pid
    }

    pub(crate) const fn pid_number(&self) -> TgidNumber {
        self.pid
    }

    pub(crate) fn identity(&self) -> Arc<PidIdentity> {
        self.identity
            .upgrade()
            .expect("process topology outlived its PID identity")
    }

    /// Returns `true` if the [`Process`] is the init process.
    ///
    /// This is a convenience method for checking if the [`Process`]
    /// [`Arc::ptr_eq`]s with the init process, which is cheaper than
    /// calling [`init_proc`] or testing if [`Process::parent`] is `None`.
    pub fn is_init(self: &Arc<Self>) -> bool {
        Arc::ptr_eq(self, INIT_PROC.get().unwrap())
    }

    /// Returns `true` if this process acts as a child subreaper.
    ///
    /// Linux keeps this flag per process: it is preserved across `execve`,
    /// applies to all threads in the thread group, and is not inherited by
    /// newly forked child processes.
    pub fn is_child_subreaper(&self) -> bool {
        self.is_child_subreaper.load(Ordering::Acquire)
    }

    /// Enables or disables child subreaper behavior for this process.
    pub fn set_child_subreaper(&self, enabled: bool) {
        self.is_child_subreaper.store(enabled, Ordering::Release);
    }
}

/// Parent & children
impl Process {
    /// The parent [`Process`].
    pub fn parent(&self) -> Option<Arc<Process>> {
        self.parent.lock_irqsave().upgrade()
    }

    /// The child [`Process`]es.
    pub fn children(&self) -> Vec<Arc<Process>> {
        self.children.lock_irqsave().values().cloned().collect()
    }
}

/// [`ProcessGroup`] & [`Session`]
impl Process {
    /// The [`ProcessGroup`] that the [`Process`] belongs to.
    pub fn group(&self) -> Arc<ProcessGroup> {
        self.group.lock_irqsave().clone()
    }

    /// Moves this process after the caller has acquired `job_control`.
    fn set_group_locked(
        self: &Arc<Self>,
        old_group: &Arc<ProcessGroup>,
        group: &Arc<ProcessGroup>,
    ) {
        if Arc::ptr_eq(old_group, group) {
            return;
        }

        if old_group.pgid_number() < group.pgid_number() {
            let mut old_members = old_group.processes.lock_irqsave();
            let mut new_members = group.processes.lock_irqsave();
            old_members.remove(&self.pid);
            new_members.insert(self.pid, self);
        } else {
            let mut new_members = group.processes.lock_irqsave();
            let mut old_members = old_group.processes.lock_irqsave();
            old_members.remove(&self.pid);
            new_members.insert(self.pid, self);
        }

        *self.group.lock_irqsave() = group.clone();
    }

    /// Creates a new [`Session`] and new [`ProcessGroup`] and moves the
    /// [`Process`] to it.
    ///
    /// If the [`Process`] is already a session leader, this method does
    /// nothing and returns `None`.
    ///
    /// Otherwise, it returns the new [`Session`] and [`ProcessGroup`].
    ///
    /// The caller has to ensure that the new [`ProcessGroup`] does not conflict
    /// with any existing [`ProcessGroup`]. Thus, the [`Process`] must not
    /// be a [`ProcessGroup`] leader.
    ///
    /// Checking [`Session`] conflicts is unnecessary.
    pub fn create_session(self: &Arc<Self>) -> Option<(Arc<Session>, Arc<ProcessGroup>)> {
        let _job_control = self.job_control.lock_irqsave();
        let old_group = self.group();
        if old_group.session.sid_number().pid_number() == self.pid.pid_number()
            || old_group.pgid_number().pid_number() == self.pid.pid_number()
        {
            return None;
        }

        let identity = self.identity();
        let new_session = Session::new(identity.clone()).ok()?;
        let new_group = ProcessGroup::get_or_create(identity, &new_session).ok()?;
        self.set_group_locked(&old_group, &new_group);

        Some((new_session, new_group))
    }

    /// Creates a new [`ProcessGroup`] and moves the [`Process`] to it.
    ///
    /// If the [`Process`] is already a group leader, this method does nothing
    /// and returns `None`.
    ///
    /// Otherwise, it returns the new [`ProcessGroup`].
    ///
    /// The caller has to ensure that the new [`ProcessGroup`] does not conflict
    /// with any existing [`ProcessGroup`].
    pub fn create_group(self: &Arc<Self>) -> Option<Arc<ProcessGroup>> {
        let _job_control = self.job_control.lock_irqsave();
        let old_group = self.group();
        if old_group.pgid_number().pid_number() == self.pid.pid_number() {
            return None;
        }

        let new_group = ProcessGroup::get_or_create(self.identity(), &old_group.session).ok()?;
        self.set_group_locked(&old_group, &new_group);

        Some(new_group)
    }

    /// Moves the [`Process`] to a specified [`ProcessGroup`].
    ///
    /// Returns `true` if the move succeeded. The move failed if the
    /// [`ProcessGroup`] is not in the same [`Session`] as the [`Process`].
    ///
    /// If the [`Process`] is already in the specified [`ProcessGroup`], this
    /// method does nothing and returns `true`.
    pub fn move_to_group(self: &Arc<Self>, group: &Arc<ProcessGroup>) -> bool {
        let _job_control = self.job_control.lock_irqsave();
        let old_group = self.group();
        if Arc::ptr_eq(&old_group, group) {
            return true;
        }

        if !Arc::ptr_eq(&old_group.session, &group.session) {
            return false;
        }

        self.set_group_locked(&old_group, group);
        true
    }
}

/// Threads
impl Process {
    /// Adds a thread to this [`Process`] with the given thread ID.
    pub fn add_thread(self: &Arc<Self>, tid: TidNumber) {
        self.tg.lock_irqsave().threads.insert(tid);
    }

    /// Removes a thread from this [`Process`], records its final CPU time, and
    /// sets the exit code if the group has not exited.
    ///
    /// The membership check, CPU-time accumulation, and last-thread decision
    /// are one transaction under the thread-group lock. Repeating an exit for
    /// the same TID therefore cannot publish process exit twice or double-count
    /// its CPU time.
    pub fn exit_thread(
        self: &Arc<Self>,
        tid: TidNumber,
        exit_code: i32,
        cpu_time: ProcessCpuTime,
    ) -> ThreadExit {
        let mut tg = self.tg.lock_irqsave();
        if !tg.threads.remove(&tid) {
            return ThreadExit::AlreadyExited;
        }
        if !tg.group_exited {
            tg.exit_code = exit_code;
        }
        tg.exited_cpu_time.add(cpu_time);
        if tg.threads.is_empty() {
            ThreadExit::Last(tg.exited_cpu_time)
        } else {
            ThreadExit::Remaining
        }
    }

    /// Get all threads in this [`Process`].
    pub fn threads(&self) -> Vec<TidNumber> {
        self.tg.lock_irqsave().threads.iter().copied().collect()
    }

    /// Renames a thread in the thread group.
    ///
    /// Used by `execve`'s de_thread step when a non-leader thread successfully
    /// `execve`s: the calling thread inherits the leader's TID so that
    /// `gettid() == getpid()` holds in the new image. We swap `old_tid` for
    /// `new_tid` atomically inside the thread-group lock so there is no
    /// instant in which the caller is unrepresented in the group.
    pub fn rename_thread(self: &Arc<Self>, old_tid: TidNumber, new_tid: TidNumber) {
        let mut tg = self.tg.lock_irqsave();
        tg.threads.remove(&old_tid);
        tg.threads.insert(new_tid);
    }

    /// Returns `true` if the [`Process`] is group exited.
    pub fn is_group_exited(&self) -> bool {
        self.tg.lock_irqsave().group_exited
    }

    /// Starts a process-wide exit if one is not already in progress.
    ///
    /// Returns a snapshot of the thread group at the point where the group-exit
    /// state was first published. Later exiting threads must not overwrite the
    /// recorded process exit code.
    pub fn start_group_exit(&self, exit_code: i32) -> Option<Vec<TidNumber>> {
        let mut tg = self.tg.lock_irqsave();
        if tg.group_exited {
            return None;
        }
        tg.group_exited = true;
        tg.exit_code = exit_code;
        Some(tg.threads.iter().copied().collect())
    }

    /// Marks the [`Process`] as group exited.
    pub fn group_exit(&self) {
        self.tg.lock_irqsave().group_exited = true;
    }

    /// The exit code of the [`Process`].
    pub fn exit_code(&self) -> i32 {
        self.tg.lock_irqsave().exit_code
    }
}

/// Process relationship transitions
impl Process {
    /// Reparents all children to `reaper`.
    ///
    /// The caller chooses the live subreaper because liveness belongs to the
    /// OS PID-identity registry, not to this relationship-only component. The
    /// selected reaper must be an ancestor of this process; that hierarchy is
    /// also the lock order for their same-class `children` locks.
    pub fn reparent_children_to(self: &Arc<Self>, reaper: &Arc<Process>) {
        if self.is_init() || Arc::ptr_eq(self, reaper) {
            return;
        }

        let reaper_parent = Arc::downgrade(reaper);

        let mut reaper_children = reaper.children.lock_irqsave();
        // The reaper and exiting process own different instances of the same
        // `children` lock class. The caller guarantees that `reaper` is an
        // ancestor, so this acquisition is structurally nested below it.
        let mut children = self
            .children
            .lock_irqsave_nested(NESTED_CHILDREN_LOCK_SUBCLASS);
        for (pid, child) in core::mem::take(&mut *children) {
            *child.parent.lock_irqsave() = reaper_parent.clone();
            reaper_children.insert(pid, child);
        }
    }

    /// Retires this process's parent and process-group links.
    ///
    /// The PID-identity state machine guarantees that exactly one consuming
    /// waiter calls this method.
    pub fn retire(self: &Arc<Self>) {
        let _job_control = self.job_control.lock_irqsave();
        let parent = self.parent();
        let group = self.group();
        let mut parent_children = parent.as_ref().map(|parent| parent.children.lock_irqsave());
        let mut group_members = group.processes.lock_irqsave();

        if let Some(children) = parent_children.as_mut()
            && children
                .get(&self.pid)
                .is_some_and(|registered| Arc::ptr_eq(registered, self))
        {
            children.remove(&self.pid);
        }
        if group_members
            .get(&self.pid)
            .is_some_and(|registered| Arc::ptr_eq(&registered, self))
        {
            group_members.remove(&self.pid);
        }
        *self.parent.lock_irqsave() = Weak::new();
    }
}

impl fmt::Debug for Process {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut builder = f.debug_struct("Process");
        builder.field("pid", &self.pid);

        let tg = self.tg.lock_irqsave();
        if tg.group_exited {
            builder.field("group_exited", &tg.group_exited);
        }
        if tg.threads.is_empty() {
            builder.field("exit_code", &tg.exit_code);
        }

        if let Some(parent) = self.parent() {
            builder.field("parent", &parent.pid());
        }
        builder.field("group", &self.group());
        builder.finish()
    }
}

/// Builder
impl Process {
    fn new_group_member(identity: Arc<PidIdentity>, parent: Option<&Arc<Process>>) -> Arc<Process> {
        let pid = TgidNumber::from(identity.root_number());
        let group = parent.map_or_else(
            || {
                let session = Session::new(identity.clone())
                    .expect("init identity must acquire its unique SID role");
                ProcessGroup::get_or_create(identity.clone(), &session)
                    .expect("init identity must acquire its unique PGID role")
            },
            |p| p.group(),
        );

        let process = Arc::new(Process {
            pid,
            identity: Arc::downgrade(&identity),
            is_child_subreaper: AtomicBool::new(false),
            tg: SpinLock::new(ThreadGroup::default()),
            children: SpinLock::new(StrongMap::new()),
            parent: SpinLock::new(parent.map(Arc::downgrade).unwrap_or_default()),
            job_control: SpinLock::new(()),
            group: SpinLock::new(group.clone()),
        });

        group.processes.lock_irqsave().insert(pid, &process);
        process
    }

    fn new(identity: Arc<PidIdentity>, parent: Option<Arc<Process>>) -> Arc<Process> {
        let pid = TgidNumber::from(identity.root_number());
        let process = Self::new_group_member(identity, parent.as_ref());

        if let Some(parent) = parent {
            parent.children.lock_irqsave().insert(pid, process.clone());
        } else {
            INIT_PROC.init_once(process.clone());
        }

        process
    }

    /// Creates a init [`Process`].
    ///
    /// This function can be called multiple times, but
    /// [`ProcessBuilder::build`] on the the result must be called only once.
    pub fn new_init(identity: Arc<PidIdentity>) -> Arc<Process> {
        Self::new(identity, None)
    }

    /// Creates a child [`Process`].
    pub fn fork(self: &Arc<Process>, identity: Arc<PidIdentity>) -> Arc<Process> {
        Self::new(identity, Some(self.clone()))
    }

    #[cfg(test)]
    pub(super) fn new_isolated_for_test(identity: Arc<PidIdentity>) -> Arc<Process> {
        Self::new_group_member(identity, None)
    }
}

static INIT_PROC: LazyInit<Arc<Process>> = LazyInit::new();

/// Gets the init process.
///
/// This function panics if the init process has not been initialized yet.
pub fn init_proc() -> Arc<Process> {
    INIT_PROC.get().unwrap().clone()
}

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use core::time::Duration;
    use std::{
        sync::{Arc as StdArc, Barrier},
        thread,
        time::Instant,
    };

    use super::{NESTED_CHILDREN_LOCK_SUBCLASS, Process};

    #[test]
    fn orphan_never_becomes_invisible_while_reparenting() {
        let namespace = crate::task::new_test_pid_namespace();
        let (init_identity, _init_tgid) = crate::task::new_test_process_identity(&namespace);
        let init = Process::new_init(init_identity);
        let (reaper_identity, _reaper_tgid) = crate::task::new_test_process_identity(&namespace);
        let reaper = init.fork(reaper_identity);
        reaper.set_child_subreaper(true);
        let (parent_identity, _parent_tgid) = crate::task::new_test_process_identity(&namespace);
        let parent = reaper.fork(parent_identity);
        let (child_identity, _child_tgid) = crate::task::new_test_process_identity(&namespace);
        let child = parent.fork(child_identity);
        let child_pid = child.pid_number();

        let reaper_children = reaper.children.lock_irqsave();
        let start_exit = StdArc::new(Barrier::new(2));
        let exit_parent = parent.clone();
        let exit_reaper = reaper.clone();
        let exit_start = start_exit.clone();
        let exit_thread = thread::spawn(move || {
            exit_start.wait();
            exit_parent.reparent_children_to(&exit_reaper);
        });

        start_exit.wait();
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut observed_invisible = false;
        while Instant::now() < deadline {
            let parent_has_child = parent
                .children
                .lock_irqsave_nested(NESTED_CHILDREN_LOCK_SUBCLASS)
                .contains_key(&child_pid);
            let reaper_has_child = reaper_children.contains_key(&child_pid);
            if !parent_has_child && !reaper_has_child {
                observed_invisible = true;
                break;
            }
            thread::yield_now();
        }

        drop(reaper_children);
        exit_thread.join().unwrap();

        assert!(
            !observed_invisible,
            "orphan was removed from its old parent before it became visible to the reaper"
        );
        assert!(Arc::ptr_eq(&reaper, &child.parent().unwrap()));
        assert!(reaper.children.lock_irqsave().contains_key(&child_pid));
    }
}
