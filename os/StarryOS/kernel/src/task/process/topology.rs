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

use super::{
    ChildRelations, GroupMoveScope, ProcessGroup, ProcessRelationTxn, RelationLock, Session,
};
use crate::{
    sync::PiMutex,
    task::{PidIdentity, TgidNumber, TidNumber},
};

type ThreadGroupLock<T> = PiMutex<T>;

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

/// Unique ownership of the final process exit selected under the thread-group
/// lock.
///
/// The owner is deliberately neither [`Clone`] nor [`Copy`]. It freezes the
/// exact process generation and wait-visible exit data at the same transition
/// that removes the final live TID.
pub struct LastThreadExitOwner {
    process: Arc<Process>,
    exit_code: i32,
    cpu_time: ProcessCpuTime,
}

impl LastThreadExitOwner {
    /// Returns the exact process generation whose final thread exited.
    pub fn process(&self) -> &Arc<Process> {
        &self.process
    }

    /// Returns the frozen Linux wait status.
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the CPU time accumulated by all threads in the process.
    pub const fn cpu_time(&self) -> ProcessCpuTime {
        self.cpu_time
    }
}

impl fmt::Debug for LastThreadExitOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LastThreadExitOwner")
            .field("pid", &self.process.pid())
            .field("exit_code", &self.exit_code)
            .field("cpu_time", &self.cpu_time)
            .finish()
    }
}

/// Result of removing one TID from a process thread group.
#[derive(Debug)]
pub enum ThreadExit {
    /// The TID had already left the thread group.
    AlreadyExited,
    /// Other threads remain alive.
    Remaining,
    /// This was the last thread and owns the final process-exit transition.
    Last(LastThreadExitOwner),
}

/// A process.
pub struct Process {
    pid: TgidNumber,
    identity: Weak<PidIdentity>,
    is_child_subreaper: AtomicBool,
    pub(crate) tg: ThreadGroupLock<ThreadGroup>,

    pub(crate) children: RelationLock<ChildRelations>,
    pub(crate) parent: RelationLock<Weak<Process>>,
    pub(crate) group: RelationLock<Arc<ProcessGroup>>,
}

/// A forked process whose topology is not visible until commit.
pub struct PreparedFork {
    process: Arc<Process>,
}

impl PreparedFork {
    pub fn process(&self) -> &Arc<Process> {
        &self.process
    }

    pub fn publish(self) -> Option<PublishedFork> {
        let process = self.process;
        ProcessRelationTxn::publish(&process).then_some(PublishedFork {
            process: Some(process),
        })
    }
}

/// Rollback token for topology published before task activation.
pub struct PublishedFork {
    process: Option<Arc<Process>>,
}

pub struct ProcessExitRelations {
    reparented_children: Vec<Arc<Process>>,
}

impl ProcessExitRelations {
    pub fn into_reparented_children(self) -> Vec<Arc<Process>> {
        self.reparented_children
    }
}

pub struct ProcessNamespaceShutdownRelations {
    retained_children: Vec<Arc<Process>>,
}

impl ProcessNamespaceShutdownRelations {
    pub fn into_retained_children(self) -> Vec<Arc<Process>> {
        self.retained_children
    }
}

impl PublishedFork {
    #[cfg(test)]
    pub fn process(&self) -> &Arc<Process> {
        self.process
            .as_ref()
            .expect("published fork token must own its process")
    }

    pub fn commit(mut self) -> Arc<Process> {
        self.process
            .take()
            .expect("published fork token must own its process")
    }
}

impl Drop for PublishedFork {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            ProcessRelationTxn::detach(&process);
        }
    }
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
    pub(crate) fn accepts_child_publication(&self) -> bool {
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
            if group.session.sid_number().pid_number() == self.pid.pid_number()
                || group.pgid_number().pid_number() == self.pid.pid_number()
            {
                return None;
            }
        }

        let identity = self.identity();
        let new_session = Session::new(identity.clone()).ok()?;
        let new_group = ProcessGroup::get_or_create(identity, &new_session).ok()?;
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
            if group.pgid_number().pid_number() == self.pid.pid_number() {
                return None;
            }
            group.session.clone()
        };
        let new_group = ProcessGroup::get_or_create(self.identity(), &session).ok()?;
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
    pub fn add_thread(self: &Arc<Self>, tid: TidNumber) {
        self.tg.lock().threads.insert(tid);
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
        let mut tg = self.tg.lock();
        if !tg.threads.remove(&tid) {
            return ThreadExit::AlreadyExited;
        }
        if !tg.group_exited {
            tg.exit_code = exit_code;
        }
        tg.exited_cpu_time.add(cpu_time);
        if tg.threads.is_empty() {
            tg.group_exited = true;
            ThreadExit::Last(LastThreadExitOwner {
                process: self.clone(),
                exit_code: tg.exit_code,
                cpu_time: tg.exited_cpu_time,
            })
        } else {
            ThreadExit::Remaining
        }
    }

    /// Get all threads in this [`Process`].
    pub fn threads(&self) -> Vec<TidNumber> {
        self.tg.lock().threads.iter().copied().collect()
    }

    /// Renames a thread in the thread group.
    ///
    /// Used by `execve`'s de_thread step when a non-leader thread successfully
    /// `execve`s: the calling thread inherits the leader's TID so that
    /// `gettid() == getpid()` holds in the new image. We swap `old_tid` for
    /// `new_tid` atomically inside the thread-group lock so there is no
    /// instant in which the caller is unrepresented in the group.
    pub fn rename_thread(self: &Arc<Self>, old_tid: TidNumber, new_tid: TidNumber) {
        let mut tg = self.tg.lock();
        tg.threads.remove(&old_tid);
        tg.threads.insert(new_tid);
    }

    /// Starts a process-wide exit if one is not already in progress.
    ///
    /// Returns a snapshot of the thread group at the point where the group-exit
    /// state was first published. Later exiting threads must not overwrite the
    /// recorded process exit code.
    pub fn start_group_exit(&self, exit_code: i32) -> Option<Vec<TidNumber>> {
        let mut tg = self.tg.lock();
        if tg.group_exited {
            return None;
        }
        tg.group_exited = true;
        tg.exit_code = exit_code;
        Some(tg.threads.iter().copied().collect())
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
    /// OS PID-identity registry, not to this relationship-only component. The
    /// selected reaper must be an ancestor of this process; that hierarchy is
    /// also the lock order for their same-class `children` locks.
    #[cfg(test)]
    pub fn reparent_children_to(self: &Arc<Self>, reaper: &Arc<Process>) {
        drop(
            self.try_begin_exit_relations(reaper)
                .expect("test reaper must accept reparented children"),
        );
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
    fn allocate(identity: Arc<PidIdentity>, parent: Option<&Arc<Process>>) -> Arc<Process> {
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

        Arc::new(Process {
            pid,
            identity: Arc::downgrade(&identity),
            is_child_subreaper: AtomicBool::new(false),
            tg: ThreadGroupLock::new(ThreadGroup::default()),
            children: RelationLock::new(ChildRelations::new()),
            parent: RelationLock::new(parent.map(Arc::downgrade).unwrap_or_default()),
            group: RelationLock::new(group),
        })
    }

    fn new(identity: Arc<PidIdentity>, parent: Option<Arc<Process>>) -> Arc<Process> {
        let process = Self::allocate(identity, parent.as_ref());

        if parent.is_some() {
            assert!(
                ProcessRelationTxn::publish(&process),
                "new child PID must not already be visible"
            );
        } else {
            ProcessRelationTxn::attach_group(&process);
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
    #[cfg(test)]
    pub fn fork(self: &Arc<Process>, identity: Arc<PidIdentity>) -> Arc<Process> {
        self.prepare_fork(identity)
            .publish()
            .expect("fork PID must not already be visible")
            .commit()
    }

    pub fn prepare_fork(self: &Arc<Process>, identity: Arc<PidIdentity>) -> PreparedFork {
        PreparedFork {
            process: Self::allocate(identity, Some(self)),
        }
    }

    /// Creates an isolated process for kernel axtests without replacing init.
    #[cfg(any(test, axtest))]
    pub(crate) fn new_for_axtest(identity: Arc<PidIdentity>) -> Arc<Process> {
        Self::new_group_member(identity, None)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{sync::Arc, vec::Vec};
    use std::{
        sync::{Arc as StdArc, Barrier},
        thread,
    };

    use super::{PreparedFork, Process, ProcessGroup};
    use crate::{
        sync::LockdepMutexExt,
        task::{PidIdentity, PidNamespaceRef, PidRoleLease, Tgid},
    };

    const NESTED_CHILDREN_LOCK_SUBCLASS: u32 = 1;
    const NESTED_GROUP_MEMBERS_LOCK_SUBCLASS: u32 = 1;

    struct TestProcessFixture {
        namespace: PidNamespaceRef,
        identities: Vec<(Arc<PidIdentity>, PidRoleLease<Tgid>)>,
    }

    impl TestProcessFixture {
        fn new() -> Self {
            Self {
                namespace: crate::task::new_test_pid_namespace(),
                identities: Vec::new(),
            }
        }

        fn identity(&mut self) -> Arc<PidIdentity> {
            let (identity, tgid) = crate::task::new_test_process_identity(&self.namespace);
            self.identities.push((identity.clone(), tgid));
            identity
        }

        fn init(&mut self) -> Arc<Process> {
            let identity = self.identity();
            Process::new_for_axtest(identity)
        }

        fn fork(&mut self, parent: &Arc<Process>) -> Arc<Process> {
            parent.fork(self.identity())
        }

        fn prepare_fork(&mut self, parent: &Arc<Process>) -> PreparedFork {
            parent.prepare_fork(self.identity())
        }
    }

    #[test]
    fn thread_group_uses_a_sleepable_pi_lock() {
        fn assert_pi_mutex<T>(_: &crate::sync::PiMutex<T>) {}

        let mut fixture = TestProcessFixture::new();
        let process = fixture.init();
        assert_pi_mutex(&process.tg);
    }

    #[test]
    fn orphan_never_becomes_invisible_while_reparenting() {
        let mut fixture = TestProcessFixture::new();
        let init = fixture.init();
        let reaper = fixture.fork(&init);
        reaper.set_child_subreaper(true);
        let parent = fixture.fork(&reaper);
        let child = fixture.fork(&parent);
        let child_pid = child.pid_number();

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
        let parent_has_child = parent
            .children
            .lock_nested(NESTED_CHILDREN_LOCK_SUBCLASS)
            .contains(child_pid.pid_number());

        drop(reaper_children);
        exit_thread.join().unwrap();

        assert!(
            parent_has_child,
            "the old parent must retain the orphan while the reaper lock blocks publication"
        );
        assert!(Arc::ptr_eq(&reaper, &child.parent().unwrap()));
        assert!(reaper.children.lock().contains(child_pid.pid_number()));
    }

    #[test]
    fn prepared_fork_is_invisible_until_publication() {
        let mut fixture = TestProcessFixture::new();
        let init = fixture.init();
        let prepared = fixture.prepare_fork(&init);
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
        let mut fixture = TestProcessFixture::new();
        let init = fixture.init();
        let prepared = fixture.prepare_fork(&init);
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
        let mut fixture = TestProcessFixture::new();
        let init = fixture.init();
        let published = fixture.prepare_fork(&init).publish().unwrap();
        let child = published.process().clone();
        let removed = child
            .group()
            .processes
            .lock()
            .remove(child.pid().pid_number());
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
        let mut fixture = TestProcessFixture::new();
        let init = fixture.init();
        let process = fixture.fork(&init);
        let source = process.group();
        let target = ProcessGroup::get_or_create(fixture.identity(), &source.session()).unwrap();
        let source_members = source.processes.lock();
        let start = StdArc::new(Barrier::new(2));
        let move_start = start.clone();
        let moving_process = process.clone();
        let moving_target = target.clone();
        let move_thread = thread::spawn(move || {
            move_start.wait();
            assert!(moving_process.move_to_group(&moving_target));
        });

        start.wait();
        let source_has_process = source_members.get(process.pid().pid_number()).is_some();
        let target_has_process = target
            .processes
            .lock_nested(NESTED_GROUP_MEMBERS_LOCK_SUBCLASS)
            .get(process.pid().pid_number())
            .is_some();

        drop(source_members);
        move_thread.join().unwrap();

        assert!(
            source_has_process && !target_has_process,
            "the source membership must remain published while its lock blocks the move"
        );
        assert!(
            source
                .processes
                .lock()
                .get(process.pid().pid_number())
                .is_none()
        );
        assert!(
            target
                .processes
                .lock()
                .get(process.pid().pid_number())
                .is_some()
        );
    }

    #[test]
    fn closed_reaper_cannot_accept_new_orphans() {
        let mut fixture = TestProcessFixture::new();
        let init = fixture.init();
        let closing_reaper = fixture.fork(&init);
        let parent = fixture.fork(&closing_reaper);
        let child = fixture.fork(&parent);

        closing_reaper.reparent_children_to(&init);
        parent.reparent_children_to(&closing_reaper);

        assert!(
            Arc::ptr_eq(&child.parent().unwrap(), &init),
            "a closed reaper accepted a child after its own exit transaction"
        );
    }

    #[test]
    fn namespace_reaper_shutdown_closes_prepared_child_publication() {
        let mut fixture = TestProcessFixture::new();
        let init = fixture.init();
        let namespace_reaper = fixture.fork(&init);
        let prepared = fixture.prepare_fork(&namespace_reaper);

        let relations = namespace_reaper.begin_namespace_shutdown_relations();

        assert!(relations.into_retained_children().is_empty());
        assert!(!namespace_reaper.accepts_child_publication());
        assert!(prepared.publish().is_none());
    }
}
