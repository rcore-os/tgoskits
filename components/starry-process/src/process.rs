use alloc::{
    collections::btree_set::BTreeSet,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    fmt,
    sync::atomic::{AtomicBool, Ordering},
};

use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use weak_map::StrongMap;

use crate::{Pid, ProcessGroup, Session};

#[derive(Default)]
pub(crate) struct ThreadGroup {
    pub(crate) threads: BTreeSet<Pid>,
    pub(crate) exit_code: i32,
    pub(crate) group_exited: bool,
}

/// A process.
pub struct Process {
    pid: Pid,
    is_zombie: AtomicBool,
    is_child_subreaper: AtomicBool,
    pub(crate) tg: SpinNoIrq<ThreadGroup>,

    children: SpinNoIrq<StrongMap<Pid, Arc<Process>>>,
    parent: SpinNoIrq<Weak<Process>>,

    group: SpinNoIrq<Arc<ProcessGroup>>,
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

    /// The child [`Process`]es.
    pub fn children(&self) -> Vec<Arc<Process>> {
        self.children.lock().values().cloned().collect()
    }
}

/// [`ProcessGroup`] & [`Session`]
impl Process {
    /// The [`ProcessGroup`] that the [`Process`] belongs to.
    pub fn group(&self) -> Arc<ProcessGroup> {
        self.group.lock().clone()
    }

    fn set_group(self: &Arc<Self>, group: &Arc<ProcessGroup>) {
        let mut self_group = self.group.lock();

        self_group.processes.lock().remove(&self.pid);

        group.processes.lock().insert(self.pid, self);

        *self_group = group.clone();
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
        if self.group.lock().session.sid() == self.pid {
            return None;
        }

        let new_session = Session::new(self.pid);
        let new_group = ProcessGroup::new(self.pid, &new_session);
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
        if self.group.lock().pgid() == self.pid {
            return None;
        }

        let new_group = ProcessGroup::new(self.pid, &self.group.lock().session);
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
        if Arc::ptr_eq(&self.group.lock(), group) {
            return true;
        }

        if !Arc::ptr_eq(&self.group.lock().session, &group.session) {
            return false;
        }

        self.set_group(group);
        true
    }
}

/// Threads
impl Process {
    /// Adds a thread to this [`Process`] with the given thread ID.
    pub fn add_thread(self: &Arc<Self>, tid: Pid) {
        self.tg.lock().threads.insert(tid);
    }

    /// Removes a thread from this [`Process`] and sets the exit code if the
    /// group has not exited.
    ///
    /// Returns `true` if this was the last thread in the process.
    pub fn exit_thread(self: &Arc<Self>, tid: Pid, exit_code: i32) -> bool {
        let mut tg = self.tg.lock();
        if !tg.group_exited {
            tg.exit_code = exit_code;
        }
        tg.threads.remove(&tid);
        tg.threads.is_empty()
    }

    /// Get all threads in this [`Process`].
    pub fn threads(&self) -> Vec<Pid> {
        self.tg.lock().threads.iter().cloned().collect()
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
        let mut tg = self.tg.lock();
        if tg.group_exited {
            return None;
        }
        tg.group_exited = true;
        tg.exit_code = exit_code;
        Some(tg.threads.iter().cloned().collect())
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

/// Status & exit
impl Process {
    fn orphan_reaper(self: &Arc<Self>) -> Arc<Process> {
        let init_proc = INIT_PROC.get().unwrap();
        let mut cursor = self.parent();

        while let Some(proc) = cursor {
            if Arc::ptr_eq(&proc, init_proc) {
                break;
            }
            if proc.is_child_subreaper() && !proc.is_zombie() {
                return proc;
            }
            cursor = proc.parent();
        }

        init_proc.clone()
    }

    /// Returns `true` if the [`Process`] is a zombie process.
    pub fn is_zombie(&self) -> bool {
        self.is_zombie.load(Ordering::Acquire)
    }

    /// Terminates the [`Process`], marking it as a zombie process.
    ///
    /// Child processes are inherited by the init process or by the nearest
    /// subreaper process.
    ///
    /// This method does nothing if the [`Process`] is the init process.
    pub fn exit(self: &Arc<Self>) {
        if self.is_init() {
            return;
        }

        let reaper_proc = self.orphan_reaper();
        let reaper_parent = Arc::downgrade(&reaper_proc);
        let children = {
            let mut children = self.children.lock();
            core::mem::take(&mut *children)
        };

        let mut reaper_children = reaper_proc.children.lock();
        for (pid, child) in children {
            *child.parent.lock() = reaper_parent.clone();
            reaper_children.insert(pid, child);
        }
        drop(reaper_children);

        self.is_zombie.store(true, Ordering::Release);
    }

    /// Frees a zombie [`Process`]. Removes it from the parent.
    ///
    /// This method panics if the [`Process`] is not a zombie.
    pub fn free(&self) {
        assert!(self.is_zombie(), "only zombie process can be freed");

        if let Some(parent) = self.parent() {
            parent.children.lock().remove(&self.pid);
        }
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
        if self.is_zombie() {
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
    fn new(pid: Pid, parent: Option<Arc<Process>>) -> Arc<Process> {
        let group = parent.as_ref().map_or_else(
            || {
                let session = Session::new(pid);
                ProcessGroup::new(pid, &session)
            },
            |p| p.group(),
        );

        let process = Arc::new(Process {
            pid,
            is_zombie: AtomicBool::new(false),
            is_child_subreaper: AtomicBool::new(false),
            tg: SpinNoIrq::new(ThreadGroup::default()),
            children: SpinNoIrq::new(StrongMap::new()),
            parent: SpinNoIrq::new(parent.as_ref().map(Arc::downgrade).unwrap_or_default()),
            group: SpinNoIrq::new(group.clone()),
        });

        group.processes.lock().insert(pid, &process);

        if let Some(parent) = parent {
            parent.children.lock().insert(pid, process.clone());
        } else {
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
        Self::new(pid, Some(self.clone()))
    }
}

static INIT_PROC: LazyInit<Arc<Process>> = LazyInit::new();

/// Gets the init process.
///
/// This function panics if the init process has not been initialized yet.
pub fn init_proc() -> Arc<Process> {
    INIT_PROC.get().unwrap().clone()
}
