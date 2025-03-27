use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use axerrno::{AxResult, ax_err};
use axsync::{Mutex, MutexGuard};

use crate::{Pgid, Pid, ProcessGroup, Session, process_group_table, process_table, session_table};

/// A process.
pub struct Process {
    pid: Pid,
    is_zombie: AtomicBool,
    exit_code: AtomicI32,
    inner: Mutex<ProcessInner>,
    // TODO: child subreaper
}

pub(crate) struct ProcessInner {
    pub(crate) children: BTreeMap<Pid, Arc<Process>>,
    pub(crate) parent: Weak<Process>,
    pub(crate) group: Weak<ProcessGroup>,
}

impl Process {
    pub(crate) fn new(pid: Pid, parent: Weak<Process>) -> Arc<Self> {
        let process = Arc::new(Self {
            pid,
            is_zombie: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            inner: Mutex::new(ProcessInner {
                children: BTreeMap::new(),
                parent,
                group: Weak::new(),
            }),
        });
        process_table().insert(pid, process.clone());
        process
    }

    pub(crate) fn inner(&self) -> MutexGuard<ProcessInner> {
        self.inner.lock()
    }

    /// The [`Process`] ID.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Create a init [`Process`].
    ///
    /// This means that the process has no parent and will have a new
    /// [`ProcessGroup`] and [`Session`].
    pub fn new_init(pid: Pid) -> Arc<Self> {
        let process = Process::new(pid, Weak::new());
        let group = ProcessGroup::new(&process);
        let _session = Session::new(&group);

        process
    }
}

/// Parent & children
impl Process {
    /// The parent [`Process`].
    pub fn parent(&self) -> Option<Arc<Process>> {
        self.inner().parent.upgrade()
    }

    /// Creates a new child [`Process`].
    pub fn new_child(self: &Arc<Self>, pid: Pid) -> Arc<Self> {
        let child = Process::new(pid, Arc::downgrade(self));
        let mut inner = self.inner();
        inner.children.insert(pid, child.clone());
        child.inner().group = inner.group.clone();
        child
    }
}

/// [`ProcessGroup`] & [`Session`]
impl Process {
    /// The [`ProcessGroup`] that the [`Process`] belongs to.
    pub fn group(&self) -> Arc<ProcessGroup> {
        // We have to guarantee that the group is always valid between two subsequent
        // public function calls so `unwrap` never fails. This means that:
        // - It cannot be `Weak::new()`.
        // - It has at least one strong reference in the global process group table.
        // So we don't expose the `Process::new` method to the public and carefully
        // manage the `group` field in each public function.
        self.inner().group.upgrade().unwrap()
    }

    /// The [`Session`] that the [`Process`] belongs to.
    pub fn session(&self) -> Arc<Session> {
        self.group().session()
    }

    /// Unlinks the [`Process`] from its [`ProcessGroup`] and [`Session`].
    ///
    /// If the [`Process`] is the last one in the [`ProcessGroup`], the
    /// [`ProcessGroup`] is removed.
    ///
    /// If `drop_session` is `true` and the [`Session`] has no more
    /// [`ProcessGroup`]s, the [`Session`] is removed.
    fn unlink(self: &Arc<Self>, drop_session: bool) {
        let group = self.group();
        let mut group_inner = group.inner();

        group_inner.processes.remove(&self.pid);

        if group_inner.processes.is_empty() {
            process_group_table().remove(&group.pgid());

            let session = group_inner.session.upgrade().unwrap();
            let mut session_inner = session.inner();

            session_inner.process_groups.remove(&group.pgid());

            if drop_session && session_inner.process_groups.is_empty() {
                session_table().remove(&session.sid());
            }
        }
    }

    /// Creates a new [`Session`] and moves the [`Process`] to it.
    ///
    /// If the [`Process`] is already a session leader, this method does
    /// nothing.
    ///
    /// Returns the new [`Session`].
    ///
    /// This method may fail if the [`Process`] ID equals any existing
    /// [`ProcessGroup`] ID. Thus, the [`Process`] must not be a
    /// [`ProcessGroup`] leader.
    ///
    /// Corresponds to the `setsid` system call.
    pub fn create_session(self: &Arc<Self>) -> AxResult<Arc<Session>> {
        if process_group_table().contains_key(&self.pid) {
            return ax_err!(PermissionDenied, "cannot create new process group");
        }

        let session = self.session();
        if session.sid() == self.pid {
            return Ok(session);
        }

        self.unlink(true);

        let new_group = ProcessGroup::new(self);
        let new_session = Session::new(&new_group);

        Ok(new_session)
    }

    /// Moves the [`Process`] to a new [`ProcessGroup`]. If the [`ProcessGroup`]
    /// does not exist and the [`Pgid`] is the same as the [`Process`] ID, a
    /// new [`ProcessGroup`] is created.
    ///
    /// If the [`Process`] is already in the specified [`ProcessGroup`], this
    /// method does nothing.
    ///
    /// This method may fail if:
    /// - The [`Process`] is a [`Session`] leader.
    /// - The [`ProcessGroup`] does not belong to the same [`Session`].
    /// - The [`ProcessGroup`] does not exist and the [`Pgid`] is not the same
    ///   as the [`Process`] ID.
    ///
    /// Corresponds to the `setpgid` system call.
    pub fn set_group(self: &Arc<Self>, pgid: Pgid) -> AxResult<()> {
        let group = self.group();
        if pgid == group.pgid() {
            return Ok(());
        }

        let session = group.session();
        if session.sid() == self.pid {
            return ax_err!(
                PermissionDenied,
                "cannot move session leader to new process group"
            );
        }

        if let Some(group) = process_group_table().get(&pgid).cloned() {
            if !session.inner().process_groups.contains_key(&pgid) {
                return ax_err!(PermissionDenied, "cannot move to a different session");
            }

            self.unlink(false);

            group.inner().processes.insert(self.pid, self.clone());
            self.inner().group = Arc::downgrade(&group);
        } else {
            if pgid != self.pid {
                return ax_err!(PermissionDenied, "process group does not exist");
            }

            self.unlink(false);

            let new_group = ProcessGroup::new(self);
            new_group.inner().session = Arc::downgrade(&session);
            session
                .inner()
                .process_groups
                .insert(new_group.pgid(), new_group);
        }
        Ok(())
    }
}

/// Status & exit
impl Process {
    /// Returns `true` if the [`Process`] is a zombie process.
    pub fn is_zombie(&self) -> bool {
        self.is_zombie.load(Ordering::Acquire)
    }

    /// The exit code of the [`Process`].
    pub fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Sets the exit code of the [`Process`].
    pub fn set_exit_code(&self, exit_code: i32) {
        self.exit_code.store(exit_code, Ordering::Release);
    }

    fn move_children_to(&self, target: &Arc<Process>) {
        let new_parent = Arc::downgrade(target);
        let mut inner = self.inner();
        let mut target_inner = target.inner();

        for (pid, child) in core::mem::take(&mut inner.children) {
            child.inner().parent = new_parent.clone();
            target_inner.children.insert(pid, child);
        }
    }

    /// Terminates the [`Process`].
    ///
    /// Child processes are inherited by the init process or by the nearest
    /// subreaper process.
    pub fn exit(&self) {
        // TODO: child subreaper

        self.is_zombie.store(true, Ordering::Release);

        // find the init process by walking up the parent chain
        let mut current = self.parent();
        let mut init = None;

        while let Some(parent) = current {
            current = parent.parent();
            init = Some(parent);
        }

        if let Some(init) = init {
            self.move_children_to(&init);
        } else {
            // TODO: init process exited!?
        }

        // the process is not removed from the process table until it is waited
    }

    /// Frees a zombie [`Process`].
    ///
    /// This method panics if the [`Process`] is not a zombie.
    pub fn free(self: &Arc<Self>) {
        assert!(self.is_zombie(), "only zombie process can be freed");

        self.unlink(true);

        process_table().remove(&self.pid);
    }
}

/// [`Process`] filter used for waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessFilter {
    Any,
    WithPid(Pid),
    WithPgid(Pgid),
}

impl ProcessFilter {
    fn apply(&self, process: &Arc<Process>) -> bool {
        match self {
            ProcessFilter::Any => true,
            ProcessFilter::WithPid(pid) => process.pid() == *pid,
            ProcessFilter::WithPgid(pgid) => process.group().pgid() == *pgid,
        }
    }
}

/// Wait
impl Process {
    pub fn find_zombie_child(&self, filter: ProcessFilter) -> AxResult<Option<Arc<Process>>> {
        let children = self
            .inner()
            .children
            .values()
            .filter(|child| filter.apply(child))
            .cloned()
            .collect::<Vec<_>>();

        if children.is_empty() {
            return ax_err!(NotFound, "no child to wait");
        }

        if let Some(zombie) = children.iter().find(|child| child.is_zombie()) {
            Ok(Some(zombie.clone()))
        } else {
            Ok(None)
        }
    }
}
