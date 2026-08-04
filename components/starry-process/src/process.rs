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

#[cfg(not(feature = "multitask"))]
use ax_kspin::SpinNoIrq as ThreadGroupLock;
use ax_lazyinit::LazyInit;
#[cfg(feature = "multitask")]
use ax_sync::PiMutex as ThreadGroupLock;

use crate::{
    Pid, ProcessGroup, Session,
    relations::{ChildRelations, GroupMoveScope, ProcessRelationTxn, RelationLock},
};

#[derive(Default)]
pub(crate) struct ThreadGroup {
    pub(crate) threads: BTreeSet<Pid>,
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
    pid: Pid,
    is_child_subreaper: AtomicBool,
    pub(crate) tg: ThreadGroupLock<ThreadGroup>,

    pub(crate) children: RelationLock<ChildRelations>,
    pub(crate) parent: RelationLock<Weak<Process>>,

    pub(crate) group: RelationLock<Arc<ProcessGroup>>,
}

/// A forked process whose parent/child and process-group links are not visible.
///
/// Clone may allocate address spaces, contexts, and Linux identities while
/// this token exists. Dropping it has no externally visible effect.
pub struct PreparedFork {
    process: Arc<Process>,
}

impl PreparedFork {
    /// Borrows the process while clone prepares its remaining resources.
    pub fn process(&self) -> &Arc<Process> {
        &self.process
    }

    /// Publishes the child into its parent and inherited process group.
    ///
    /// Publication fails without changing either collection if the PID was
    /// reused while clone was preparing resources.
    pub fn publish(self) -> Option<PublishedFork> {
        let process = self.process;
        if !ProcessRelationTxn::publish(&process) {
            return None;
        }
        Some(PublishedFork {
            process: Some(process),
        })
    }
}

/// Rollback token for a fork published before its scheduler thread is runnable.
pub struct PublishedFork {
    process: Option<Arc<Process>>,
}

/// Children frozen and reparented by one process-exit relationship transaction.
pub struct ProcessExitRelations {
    reparented_children: Vec<Arc<Process>>,
}

impl ProcessExitRelations {
    /// Consumes the transaction result and returns the exact former children.
    pub fn into_reparented_children(self) -> Vec<Arc<Process>> {
        self.reparented_children
    }
}

/// Children retained by a PID namespace reaper while its namespace shuts down.
///
/// New fork publication is closed by the transaction, while existing
/// descendants remain attached so the namespace reaper can terminate and reap
/// them before its own identity becomes externally reapable.
pub struct ProcessNamespaceShutdownRelations {
    retained_children: Vec<Arc<Process>>,
}

impl ProcessNamespaceShutdownRelations {
    /// Consumes the transaction result and returns the exact retained children.
    pub fn into_retained_children(self) -> Vec<Arc<Process>> {
        self.retained_children
    }
}

impl PublishedFork {
    /// Borrows the published child.
    pub fn process(&self) -> &Arc<Process> {
        self.process
            .as_ref()
            .expect("published fork token must own its process")
    }

    /// Transfers the child to the normal process exit and reap lifecycle.
    pub fn commit(mut self) -> Arc<Process> {
        self.process
            .take()
            .expect("published fork token must own its process")
    }
}

impl Drop for PublishedFork {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            process.rollback_fork_publication();
        }
    }
}

impl Process {
    /// The [`Process`] ID.
    pub fn pid(&self) -> Pid {
        self.pid
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
        self.parent.lock().upgrade()
    }

    /// Returns whether this process can still accept a newly published child.
    ///
    /// This is an advisory snapshot. A caller that reparents children must use
    /// [`Self::try_begin_exit_relations`] to commit against the same state.
    pub fn accepts_child_publication(&self) -> bool {
        self.children.lock().is_open()
    }

    /// The child [`Process`]es.
    pub fn children(&self) -> Vec<Arc<Process>> {
        loop {
            let child_count = self.children.lock().len();
            let mut children = Vec::with_capacity(child_count);
            let relations = self.children.lock();
            if children.capacity() < relations.len() {
                drop(relations);
                continue;
            }
            relations.snapshot(&mut children);
            return children;
        }
    }
}

/// [`ProcessGroup`] & [`Session`]
impl Process {
    /// The [`ProcessGroup`] that the [`Process`] belongs to.
    pub fn group(&self) -> Arc<ProcessGroup> {
        self.group.lock().clone()
    }

    fn set_group(self: &Arc<Self>, group: &Arc<ProcessGroup>) {
        assert!(ProcessRelationTxn::move_group(
            self,
            group,
            GroupMoveScope::AnySession,
        ));
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
        {
            let group = self.group.lock();
            if group.session.sid() == self.pid {
                return None;
            }
        }

        let new_session = Session::new(self.pid);
        let new_group = ProcessGroup::get_or_create(self.pid, &new_session);
        self.set_group(&new_group);

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
        let session = {
            let group = self.group.lock();
            if group.pgid() == self.pid {
                return None;
            }
            group.session.clone()
        };

        let new_group = ProcessGroup::get_or_create(self.pid, &session);
        self.set_group(&new_group);

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
        ProcessRelationTxn::move_group(self, group, GroupMoveScope::SameSession)
    }
}

/// Threads
impl Process {
    /// Adds a thread to this [`Process`] with the given thread ID.
    pub fn add_thread(self: &Arc<Self>, tid: Pid) {
        self.tg.lock().threads.insert(tid);
    }

    /// Removes a thread that was registered for a child not yet published.
    ///
    /// Unlike [`Self::exit_thread`], this rollback operation does not alter
    /// process exit state. It must only be used while the child cannot run.
    #[must_use]
    pub fn remove_unpublished_thread(self: &Arc<Self>, tid: Pid) -> bool {
        let mut tg = self.tg.lock();
        assert!(
            !tg.group_exited,
            "cannot roll back a thread after group exit started"
        );
        tg.threads.remove(&tid)
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
        tid: Pid,
        exit_code: i32,
        cpu_time: ProcessCpuTime,
    ) -> ThreadExit {
        let mut tg = self.tg.lock();
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
    pub fn threads(&self) -> Vec<Pid> {
        loop {
            let thread_count = self.tg.lock().threads.len();
            let mut threads = Vec::with_capacity(thread_count);
            let thread_group = self.tg.lock();
            if threads.capacity() < thread_group.threads.len() {
                drop(thread_group);
                continue;
            }
            threads.extend(thread_group.threads.iter().copied());
            return threads;
        }
    }

    /// Renames a thread in the thread group.
    ///
    /// Used by `execve`'s de_thread step when a non-leader thread successfully
    /// `execve`s: the calling thread inherits the leader's TID so that
    /// `gettid() == getpid()` holds in the new image. We swap `old_tid` for
    /// `new_tid` atomically inside the thread-group lock so there is no
    /// instant in which the caller is unrepresented in the group.
    pub fn rename_thread(self: &Arc<Self>, old_tid: Pid, new_tid: Pid) {
        let mut tg = self.tg.lock();
        tg.threads.remove(&old_tid);
        tg.threads.insert(new_tid);
    }

    /// Returns `true` if the [`Process`] is group exited.
    pub fn is_group_exited(&self) -> bool {
        self.tg.lock().group_exited
    }

    /// Starts a process-wide exit if one is not already in progress.
    ///
    /// Returns a snapshot of the thread group at the point where the group-exit
    /// state was first published. Later exiting threads must not overwrite the
    /// recorded process exit code.
    pub fn start_group_exit(&self, exit_code: i32) -> Option<Vec<Pid>> {
        loop {
            let thread_count = self.tg.lock().threads.len();
            let mut threads = Vec::with_capacity(thread_count);
            let mut thread_group = self.tg.lock();
            if thread_group.group_exited {
                return None;
            }
            if threads.capacity() < thread_group.threads.len() {
                drop(thread_group);
                continue;
            }
            thread_group.group_exited = true;
            thread_group.exit_code = exit_code;
            threads.extend(thread_group.threads.iter().copied());
            return Some(threads);
        }
    }

    /// Marks the [`Process`] as group exited.
    pub fn group_exit(&self) {
        self.tg.lock().group_exited = true;
    }

    /// The exit code of the [`Process`].
    pub fn exit_code(&self) -> i32 {
        self.tg.lock().exit_code
    }
}

/// Process relationship transitions
impl Process {
    /// Tries to close child publication and reparent all existing children.
    ///
    /// Returns `None` if `reaper` completed its own relationship exit before
    /// this transaction acquired both child sets. The caller should choose a
    /// new live ancestor and retry.
    pub fn try_begin_exit_relations(
        self: &Arc<Self>,
        reaper: &Arc<Process>,
    ) -> Option<ProcessExitRelations> {
        Some(ProcessExitRelations {
            reparented_children: ProcessRelationTxn::begin_exit(self, reaper)?,
        })
    }

    /// Closes child publication and reparents all existing children to
    /// `reaper`.
    ///
    /// This is the relationship half of the process exit transaction. Once it
    /// returns, every prepared fork that has not yet published is rejected.
    /// The returned snapshot is exactly the set moved to `reaper`, so callers
    /// can deliver parent-death notifications without a snapshot/reparent race.
    pub fn begin_exit_relations(self: &Arc<Self>, reaper: &Arc<Process>) -> ProcessExitRelations {
        self.try_begin_exit_relations(reaper).unwrap_or_else(|| {
            self.try_begin_exit_relations(&init_proc())
                .expect("init process must remain available as orphan reaper")
        })
    }

    /// Closes new child publication while retaining existing descendants.
    ///
    /// PID namespace shutdown uses this transaction before it terminates the
    /// remaining namespace members. Unlike normal exit, retained children are
    /// not exposed to a reaper outside the namespace.
    pub fn begin_namespace_shutdown_relations(
        self: &Arc<Self>,
    ) -> ProcessNamespaceShutdownRelations {
        ProcessNamespaceShutdownRelations {
            retained_children: ProcessRelationTxn::begin_namespace_shutdown(self),
        }
    }

    /// Reparents all children to `reaper`.
    ///
    /// The caller chooses the live subreaper because liveness belongs to the
    /// OS PID-identity registry, not to this relationship-only component.
    pub fn reparent_children_to(self: &Arc<Self>, reaper: &Arc<Process>) {
        drop(self.begin_exit_relations(reaper));
    }

    /// Retires this process's parent and process-group links.
    ///
    /// The PID-identity state machine guarantees that exactly one consuming
    /// waiter calls this method.
    pub fn retire(self: &Arc<Self>) {
        ProcessRelationTxn::detach(self);
    }
}

impl fmt::Debug for Process {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut builder = f.debug_struct("Process");
        builder.field("pid", &self.pid);

        let tg = self.tg.lock();
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
    fn allocate(pid: Pid, parent: Option<Arc<Process>>) -> Arc<Process> {
        let group = parent.as_ref().map_or_else(
            || {
                let session = Session::new(pid);
                ProcessGroup::get_or_create(pid, &session)
            },
            |p| p.group(),
        );

        Arc::new(Process {
            pid,
            is_child_subreaper: AtomicBool::new(false),
            tg: ThreadGroupLock::new(ThreadGroup::default()),
            children: RelationLock::new(ChildRelations::new()),
            parent: RelationLock::new(parent.as_ref().map(Arc::downgrade).unwrap_or_default()),
            group: RelationLock::new(group.clone()),
        })
    }

    fn new(pid: Pid, parent: Option<Arc<Process>>) -> Arc<Process> {
        let process = Self::allocate(pid, parent.clone());
        if parent.is_some() {
            assert!(
                ProcessRelationTxn::publish(&process),
                "new child PID must not already be visible"
            );
        } else {
            ProcessRelationTxn::attach_group(&process);
            INIT_PROC.init_once(process.clone());
        }
        process
    }

    /// Creates a init [`Process`].
    ///
    /// This function can be called multiple times, but
    /// [`ProcessBuilder::build`] on the the result must be called only once.
    pub fn new_init(pid: Pid) -> Arc<Process> {
        Self::new(pid, None)
    }

    /// Creates a child [`Process`].
    pub fn fork(self: &Arc<Process>, pid: Pid) -> Arc<Process> {
        self.prepare_fork(pid)
            .publish()
            .expect("fork PID must not already be visible")
            .commit()
    }

    /// Allocates a child without publishing it to parent or group observers.
    pub fn prepare_fork(self: &Arc<Process>, pid: Pid) -> PreparedFork {
        PreparedFork {
            process: Self::allocate(pid, Some(self.clone())),
        }
    }

    fn rollback_fork_publication(self: &Arc<Process>) {
        ProcessRelationTxn::detach(self);
    }

    /// Creates an isolated process for kernel axtests without replacing init.
    #[cfg(axtest)]
    pub fn new_for_axtest(pid: Pid) -> Arc<Process> {
        let process = Self::allocate(pid, None);
        ProcessRelationTxn::attach_group(&process);
        process
    }
}

static INIT_PROC: LazyInit<Arc<Process>> = LazyInit::new();

/// Gets the init process.
///
/// This function panics if the init process has not been initialized yet.
pub fn init_proc() -> Arc<Process> {
    INIT_PROC.get().unwrap().clone()
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use core::time::Duration;
    use std::{
        sync::{Arc as StdArc, Barrier, OnceLock},
        thread,
        time::Instant,
    };

    use super::Process;
    use crate::ProcessGroup;

    fn test_init() -> Arc<Process> {
        static TEST_INIT: OnceLock<Arc<Process>> = OnceLock::new();
        TEST_INIT.get_or_init(|| Process::new_init(1)).clone()
    }

    #[cfg(feature = "multitask")]
    #[test]
    fn multitask_thread_group_uses_a_sleepable_pi_lock() {
        fn assert_pi_mutex<T>(_: &ax_sync::PiMutex<T>) {}

        let process = test_init();
        assert_pi_mutex(&process.tg);
    }

    #[test]
    fn orphan_never_becomes_invisible_while_reparenting() {
        let init = test_init();
        let reaper = init.fork(2);
        reaper.set_child_subreaper(true);
        let parent = reaper.fork(3);
        let child = parent.fork(4);
        let child_pid = child.pid();

        let reaper_children = reaper.children.lock();
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
            let parent_has_child = parent.children.lock().contains(child_pid);
            let reaper_has_child = reaper_children.contains(child_pid);
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
        assert!(reaper.children.lock().contains(child_pid));
    }

    #[test]
    fn prepared_fork_is_invisible_until_publication() {
        let init = test_init();
        let prepared = init.prepare_fork(12);
        let child = prepared.process();

        assert!(!init.children().iter().any(|proc| Arc::ptr_eq(proc, child)));
        assert!(
            !child
                .group()
                .processes()
                .iter()
                .any(|proc| Arc::ptr_eq(proc, child))
        );

        let published = prepared.publish().unwrap();
        let child = published.process().clone();
        assert!(init.children().iter().any(|proc| Arc::ptr_eq(proc, &child)));
        assert!(
            child
                .group()
                .processes()
                .iter()
                .any(|proc| Arc::ptr_eq(proc, &child))
        );
        published.commit();
    }

    #[test]
    fn dropping_prepared_fork_leaves_parent_and_group_unchanged() {
        let init = test_init();
        let prepared = init.prepare_fork(13);
        let child = prepared.process().clone();
        drop(prepared);

        assert!(!init.children().iter().any(|proc| Arc::ptr_eq(proc, &child)));
        assert!(
            !child
                .group()
                .processes()
                .iter()
                .any(|proc| Arc::ptr_eq(proc, &child))
        );
    }

    #[test]
    fn published_fork_rollback_repairs_a_partially_removed_identity() {
        let init = test_init();
        let published = init.prepare_fork(14).publish().unwrap();
        let child = published.process().clone();
        let removed = child.group().processes.lock().remove(child.pid());
        drop(removed);

        drop(published);

        assert!(child.parent().is_none());
        assert!(
            !init
                .children()
                .iter()
                .any(|process| Arc::ptr_eq(process, &child))
        );
        assert!(
            !child
                .group()
                .processes()
                .iter()
                .any(|process| Arc::ptr_eq(process, &child))
        );
    }

    #[test]
    fn group_move_never_makes_process_temporarily_invisible() {
        let init = test_init();
        let process = init.fork(91);
        let source = process.group();
        let target = ProcessGroup::get_or_create(92, &source.session());
        let target_members = target.processes.lock();
        let start = StdArc::new(Barrier::new(2));
        let move_start = start.clone();
        let moving_process = process.clone();
        let moving_target = target.clone();
        let move_thread = thread::spawn(move || {
            move_start.wait();
            assert!(moving_process.move_to_group(&moving_target));
        });

        start.wait();
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut observed_invisible = false;
        while Instant::now() < deadline {
            let source_has_process = source.processes.lock().get(process.pid()).is_some();
            let target_has_process = target_members.get(process.pid()).is_some();
            if !source_has_process && !target_has_process {
                observed_invisible = true;
                break;
            }
            thread::yield_now();
        }

        drop(target_members);
        move_thread.join().unwrap();

        assert!(
            !observed_invisible,
            "group move removed the process before the destination membership was reserved"
        );
        assert!(source.processes.lock().get(process.pid()).is_none());
        assert!(target.processes.lock().get(process.pid()).is_some());
    }

    #[test]
    fn closed_reaper_cannot_accept_new_orphans() {
        let init = test_init();
        let closing_reaper = init.fork(101);
        let parent = closing_reaper.fork(102);
        let child = parent.fork(103);

        closing_reaper.reparent_children_to(&init);
        parent.reparent_children_to(&closing_reaper);

        assert!(
            Arc::ptr_eq(&child.parent().unwrap(), &init),
            "a closed reaper accepted a child after its own exit transaction"
        );
    }

    #[test]
    fn namespace_reaper_shutdown_closes_prepared_child_publication() {
        let init = test_init();
        let namespace_reaper = init.fork(104);
        let prepared = namespace_reaper.prepare_fork(105);

        let relations = namespace_reaper.begin_namespace_shutdown_relations();

        assert!(relations.into_retained_children().is_empty());
        assert!(!namespace_reaper.accepts_child_publication());
        assert!(prepared.publish().is_none());
    }
}
