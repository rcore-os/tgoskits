use alloc::{
    collections::btree_map::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use kspin::{SpinNoIrq, SpinNoIrqGuard};

use crate::{Pid, ProcessGroup, Session};

// FIXME: This should be a `Tid` counter after we implement threads.
static PID_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Allocates a new [`Pid`].
pub fn alloc_pid() -> Pid {
    PID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

/// A process.
pub struct Process {
    pid: Pid,
    exit_code: AtomicI32,
    inner: SpinNoIrq<ProcessInner>,
    // TODO: child subreaper
}

pub(crate) struct ProcessInner {
    is_zombie: bool,
    children: BTreeMap<Pid, Arc<Process>>,
    parent: Weak<Process>,
    group: Arc<ProcessGroup>,
}

impl ProcessInner {
    fn move_children_to(&mut self, target: &Arc<Process>) {
        let new_parent = Arc::downgrade(target);
        let mut target_inner = target.inner();

        for (pid, child) in core::mem::take(&mut self.children) {
            child.inner().parent = new_parent.clone();
            target_inner.children.insert(pid, child);
        }
    }
}

impl Process {
    pub(crate) fn new(pid: Pid, parent: Weak<Process>, group: &Arc<ProcessGroup>) -> Arc<Self> {
        let process = Arc::new(Self {
            pid,
            exit_code: AtomicI32::new(0),
            inner: SpinNoIrq::new(ProcessInner {
                is_zombie: false,
                children: BTreeMap::new(),
                parent,
                group: group.clone(),
            }),
        });
        group
            .inner()
            .processes
            .insert(pid, Arc::downgrade(&process));
        process
    }

    pub(crate) fn inner(&self) -> SpinNoIrqGuard<ProcessInner> {
        self.inner.lock()
    }
}

impl Process {
    /// The [`Process`] ID.
    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Create a init [`Process`].
    ///
    /// This means that the process has no parent and will have a new
    /// [`ProcessGroup`] and [`Session`].
    pub fn new_init() -> Arc<Self> {
        let pid = alloc_pid();
        let session = Session::new(pid);
        let group = ProcessGroup::new(pid, &session);

        Process::new(pid, Weak::new(), &group)
    }
}

/// Parent & children
impl Process {
    /// The parent [`Process`].
    pub fn parent(&self) -> Option<Arc<Process>> {
        self.inner().parent.upgrade()
    }

    /// The child [`Process`]es.
    pub fn children(&self) -> Vec<Arc<Process>> {
        self.inner().children.values().cloned().collect()
    }

    /// Creates a new child [`Process`].
    pub fn fork(self: &Arc<Self>) -> Arc<Self> {
        let mut inner = self.inner();
        let pid = alloc_pid();
        let child = Process::new(pid, Arc::downgrade(self), &inner.group);
        inner.children.insert(pid, child.clone());
        child
    }
}

/// [`ProcessGroup`] & [`Session`]
impl Process {
    /// The [`ProcessGroup`] that the [`Process`] belongs to.
    pub fn group(&self) -> Arc<ProcessGroup> {
        self.inner().group.clone()
    }

    fn set_group(self: &Arc<Self>, group: &Arc<ProcessGroup>) {
        let mut inner = self.inner();

        inner.group.inner().processes.remove(&self.pid);

        group
            .inner()
            .processes
            .insert(self.pid, Arc::downgrade(self));

        inner.group = group.clone();
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
        if self.inner().group.inner().session.sid() == self.pid {
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
        if self.inner().group.pgid() == self.pid {
            return None;
        }

        let new_group = ProcessGroup::new(self.pid, &self.inner().group.inner().session);
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
        if Arc::ptr_eq(&self.inner().group, group) {
            return true;
        }

        if !Arc::ptr_eq(&self.inner().group.inner().session, &group.inner().session) {
            return false;
        }

        self.set_group(group);
        true
    }
}

/// Status & exit
impl Process {
    /// The exit code of the [`Process`].
    pub fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Sets the exit code of the [`Process`].
    pub fn set_exit_code(&self, exit_code: i32) {
        self.exit_code.store(exit_code, Ordering::Release);
    }

    /// Returns `true` if the [`Process`] is a zombie process.
    pub fn is_zombie(&self) -> bool {
        self.inner().is_zombie
    }

    /// Terminates the [`Process`].
    ///
    /// Child processes are inherited by the init process or by the nearest
    /// subreaper process.
    pub fn exit(&self) {
        // TODO: child subreaper

        // find the init process by walking up the parent chain
        let mut current = self.parent();
        let mut init = None;

        while let Some(parent) = current {
            current = parent.parent();
            init = Some(parent);
        }

        let mut inner = self.inner();
        inner.is_zombie = true;

        if let Some(init) = init {
            inner.move_children_to(&init);
        } else {
            // TODO: init process exited!?
        }
    }

    /// Frees a zombie [`Process`]. Removes it from the parent.
    ///
    /// This method panics if the [`Process`] is not a zombie.
    pub fn free(self: &Arc<Self>) {
        assert!(self.is_zombie(), "only zombie process can be freed");

        if let Some(parent) = self.parent() {
            parent.inner().children.remove(&self.pid);
        }
    }
}
