use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
};

use axerrno::{AxResult, ax_err};
use axsync::{Mutex, MutexGuard};

use crate::{Pgid, Pid, ProcessGroup, Session, process_group_table, process_table, session_table};

/// A process.
pub struct Process {
    pid: Pid,
    inner: Mutex<ProcessInner>,
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

    fn orphan(self: &Arc<Self>, drop_session: bool) {
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

        self.orphan(true);

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

            self.orphan(false);

            group.inner().processes.insert(self.pid, self.clone());
            self.inner().group = Arc::downgrade(&group);
        } else {
            if pgid != self.pid {
                return ax_err!(PermissionDenied, "process group does not exist");
            }

            self.orphan(false);

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
