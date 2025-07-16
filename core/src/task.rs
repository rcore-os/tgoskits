//! User task management.

mod stat;

use alloc::{
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    cell::RefCell,
    ops::Deref,
    sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering},
    time::Duration,
};

use axerrno::{LinuxError, LinuxResult};
use axhal::arch::UspaceContext;
use axmm::AddrSpace;
use axprocess::{Pid, Process, ProcessGroup, Session, Thread};
use axsignal::{
    SignalInfo, Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};
use axsync::{Mutex, RawMutex};
use axtask::{AxTaskRef, TaskExt, TaskInner, WaitQueue, WeakAxTaskRef, current};
use event_listener::Event;
use extern_trait::extern_trait;
use linux_raw_sys::general::SI_KERNEL;
use scope_local::{ActiveScope, Scope};
use spin::{Once, RwLock};
pub use stat::TaskStat;
use weak_map::WeakMap;

use crate::{
    futex::{FutexKey, FutexTable},
    mm::access_user_memory,
    resources::Rlimits,
    time::{TimeManager, TimerState},
};

/// Create a new user task.
pub fn new_user_task(
    name: &str,
    uctx: UspaceContext,
    set_child_tid: Option<&'static mut Pid>,
) -> TaskInner {
    TaskInner::new(
        move || {
            let curr = axtask::current();
            access_user_memory(|| {
                if let Some(tid) = set_child_tid {
                    *tid = curr.id().as_u64() as Pid;
                }
            });

            let kstack_top = curr.kernel_stack_top().unwrap();
            info!(
                "Enter user space: entry={:#x}, ustack={:#x}, kstack={:#x}",
                uctx.ip(),
                uctx.sp(),
                kstack_top,
            );
            unsafe { uctx.enter_uspace(kstack_top) }
        },
        name.into(),
        crate::config::KERNEL_STACK_SIZE,
    )
}

/// Task extended data for the monolithic kernel.
pub struct StarryTaskExt {
    /// The thread associated with this task.
    pub thread: Arc<Thread>,
}

#[extern_trait]
unsafe impl TaskExt for StarryTaskExt {
    fn on_enter(&self) {
        let scope = self.process_data().scope.read();
        unsafe { ActiveScope::set(&scope) };
        core::mem::forget(scope);
    }

    fn on_leave(&self) {
        ActiveScope::set_global();
        unsafe { self.process_data().scope.force_read_decrement() };
    }
}

impl StarryTaskExt {
    /// Create a new [`StarryTaskExt`].
    pub fn new(thread: Arc<Thread>) -> Self {
        Self { thread }
    }

    /// Convenience function for getting the extended data for a task.
    /// # Panics
    /// Panics if the current task is a kernel task.
    pub fn of(task: &TaskInner) -> &Self {
        Self::try_of(task).unwrap()
    }

    /// Convenience function for trying to get the extended data for a task.
    pub fn try_of(task: &TaskInner) -> Option<&Self> {
        task.task_ext().map(|ext| unsafe { ext.downcast_ref() })
    }

    /// Get the [`ThreadData`] associated with this task.
    pub fn thread_data(&self) -> &ThreadData {
        self.thread.data().unwrap()
    }

    /// Get the [`ProcessData`] associated with this task.
    pub fn process_data(&self) -> &ProcessData {
        self.thread.process().data().unwrap()
    }
}

#[doc(hidden)]
pub struct WaitQueueWrapper(WaitQueue);

impl Default for WaitQueueWrapper {
    fn default() -> Self {
        Self(WaitQueue::new())
    }
}

impl axsignal::api::WaitQueue for WaitQueueWrapper {
    fn wait_timeout(&self, timeout: Option<Duration>) -> bool {
        if let Some(timeout) = timeout {
            self.0.wait_timeout(timeout)
        } else {
            self.0.wait();
            true
        }
    }

    fn notify_one(&self) -> bool {
        self.0.notify_one(false)
    }
}

///  A wrapper type that assumes the inner type is `Sync`.
#[repr(transparent)]
pub struct AssumeSync<T>(T);

unsafe impl<T> Sync for AssumeSync<T> {}

impl<T> Deref for AssumeSync<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Extended data for [`Thread`].
pub struct ThreadData {
    /// Weak reference to the associated task.
    task: Arc<Once<WeakAxTaskRef>>,

    /// The clear thread tid field
    ///
    /// See <https://manpages.debian.org/unstable/manpages-dev/set_tid_address.2.en.html#clear_child_tid>
    ///
    /// When the thread exits, the kernel clears the word at this address if it
    /// is not NULL.
    pub clear_child_tid: AtomicUsize,

    /// The head of the robust list
    pub robust_list_head: AtomicUsize,

    /// The thread-level signal manager
    pub signal: ThreadSignalManager<RawMutex, WaitQueueWrapper>,

    /// Time manager
    ///
    /// This is assumed to be `Sync` because it's only borrowed mutably during
    /// context switches, which is exclusive to the current thread.
    pub time: AssumeSync<RefCell<TimeManager>>,

    /// The bitset used for futex operations (FUTEX_{WAIT,WAKE}_BITSET).
    pub futex_bitset: AtomicU32,

    /// The OOM score adjustment value.
    pub oom_score_adj: AtomicI32,
}

impl ThreadData {
    /// Create a new [`ThreadData`].
    #[allow(clippy::new_without_default)]
    pub fn new(proc: &ProcessData) -> Self {
        Self {
            task: Arc::new(Once::new()),

            clear_child_tid: AtomicUsize::new(0),
            robust_list_head: AtomicUsize::new(0),

            signal: ThreadSignalManager::new(proc.signal.clone()),

            time: AssumeSync(RefCell::new(TimeManager::new())),

            futex_bitset: AtomicU32::new(0),

            oom_score_adj: AtomicI32::new(200),
        }
    }

    /// Get the clear child tid field.
    pub fn clear_child_tid(&self) -> usize {
        self.clear_child_tid.load(Ordering::Relaxed)
    }

    /// Set the clear child tid field.
    pub fn set_clear_child_tid(&self, clear_child_tid: usize) {
        self.clear_child_tid
            .store(clear_child_tid, Ordering::Relaxed);
    }

    /// Initialize the task reference.
    pub fn init_task(&self, f: impl FnOnce() -> WeakAxTaskRef) {
        self.task.call_once(f);
    }

    /// Get the task reference.
    pub fn get_task(&self) -> LinuxResult<AxTaskRef> {
        self.task
            .get()
            .and_then(Weak::upgrade)
            .ok_or(LinuxError::ESRCH)
    }

    /// Poll the timer for this thread.
    pub fn poll_timer(&self) {
        let Ok(mut time) = self.time.try_borrow_mut() else {
            // reentrant borrow, likely IRQ
            return;
        };
        time.poll(|signo| {
            self.signal
                .send_signal(SignalInfo::new(signo, SI_KERNEL as _));
            // TODO(mivik):  correct interruption handling
            current().set_interrupted(true);
        });
    }

    /// Update the timer state for this thread.
    pub fn set_timer_state(&self, state: TimerState) {
        let Ok(mut time) = self.time.try_borrow_mut() else {
            return;
        };
        time.poll(|signo| {
            self.signal
                .send_signal(SignalInfo::new(signo, SI_KERNEL as _));
            current().set_interrupted(true);
        });
        time.set_state(state);
    }
}

/// Extended data for [`Process`].
pub struct ProcessData {
    /// The executable path
    pub exe_path: RwLock<String>,
    /// The virtual memory address space.
    pub aspace: Arc<Mutex<AddrSpace>>,
    /// The resource scope
    pub scope: RwLock<Scope>,
    /// The user heap bottom
    heap_bottom: AtomicUsize,
    /// The user heap top
    heap_top: AtomicUsize,

    /// The resource limits
    pub rlim: RwLock<Rlimits>,

    /// The child exit wait event
    pub child_exit_event: Event,
    /// The exit signal of the thread
    pub exit_signal: Option<Signo>,

    /// The process signal manager
    pub signal: Arc<ProcessSignalManager<RawMutex, WaitQueueWrapper>>,

    /// The futex table.
    futex_table: FutexTable,

    /// The default mask for file permissions.
    pub umask: AtomicU32,
}

impl ProcessData {
    /// Create a new [`ProcessData`].
    pub fn new(
        exe_path: String,
        aspace: Arc<Mutex<AddrSpace>>,
        signal_actions: Arc<Mutex<SignalActions>>,
        exit_signal: Option<Signo>,
    ) -> Self {
        Self {
            exe_path: RwLock::new(exe_path),
            aspace,
            scope: RwLock::new(Scope::new()),
            heap_bottom: AtomicUsize::new(crate::config::USER_HEAP_BASE),
            heap_top: AtomicUsize::new(crate::config::USER_HEAP_BASE),

            rlim: RwLock::default(),

            child_exit_event: Event::new(),
            exit_signal,

            signal: Arc::new(ProcessSignalManager::new(
                signal_actions,
                crate::config::SIGNAL_TRAMPOLINE,
            )),

            futex_table: FutexTable::new(),

            umask: AtomicU32::new(0o022),
        }
    }

    /// Get the bottom address of the user heap.
    pub fn get_heap_bottom(&self) -> usize {
        self.heap_bottom.load(Ordering::Acquire)
    }

    /// Set the bottom address of the user heap.
    pub fn set_heap_bottom(&self, bottom: usize) {
        self.heap_bottom.store(bottom, Ordering::Release)
    }

    /// Get the top address of the user heap.
    pub fn get_heap_top(&self) -> usize {
        self.heap_top.load(Ordering::Acquire)
    }

    /// Set the top address of the user heap.
    pub fn set_heap_top(&self, top: usize) {
        self.heap_top.store(top, Ordering::Release)
    }

    /// Linux manual: A "clone" child is one which delivers no signal, or a
    /// signal other than SIGCHLD to its parent upon termination.
    pub fn is_clone_child(&self) -> bool {
        self.exit_signal != Some(Signo::SIGCHLD)
    }

    /// Returns the futex table for the given key.
    pub fn futex_table_for(&self, key: &FutexKey) -> &FutexTable {
        match key {
            FutexKey::Private { .. } => &self.futex_table,
            FutexKey::Shared { .. } => &SHARED_FUTEX_TABLE,
        }
    }
}

static SHARED_FUTEX_TABLE: FutexTable = FutexTable::new();

static THREAD_TABLE: RwLock<WeakMap<Pid, Weak<Thread>>> = RwLock::new(WeakMap::new());

static PROCESS_TABLE: RwLock<WeakMap<Pid, Weak<Process>>> = RwLock::new(WeakMap::new());

static PROCESS_GROUP_TABLE: RwLock<WeakMap<Pid, Weak<ProcessGroup>>> = RwLock::new(WeakMap::new());

static SESSION_TABLE: RwLock<WeakMap<Pid, Weak<Session>>> = RwLock::new(WeakMap::new());

/// Cleanup expired entries in the task tables.
///
/// This function is intended to be used during memory leak analysis to remove
/// possible noise caused by expired entries in the [`WeakMap`].
#[cfg(feature = "track")]
pub(crate) fn cleanup_task_tables() {
    THREAD_TABLE.write().cleanup();
    PROCESS_TABLE.write().cleanup();
    PROCESS_GROUP_TABLE.write().cleanup();
    SESSION_TABLE.write().cleanup();
}

/// Add the thread and possibly its process, process group and session to the
/// corresponding tables.
pub fn add_thread_to_table(thread: &Arc<Thread>) {
    let mut thread_table = THREAD_TABLE.write();
    thread_table.insert(thread.tid(), thread);

    let mut process_table = PROCESS_TABLE.write();
    let process = thread.process();
    if process_table.contains_key(&process.pid()) {
        return;
    }
    process_table.insert(process.pid(), process);

    let mut process_group_table = PROCESS_GROUP_TABLE.write();
    let process_group = process.group();
    if process_group_table.contains_key(&process_group.pgid()) {
        return;
    }
    process_group_table.insert(process_group.pgid(), &process_group);

    let mut session_table = SESSION_TABLE.write();
    let session = process_group.session();
    if session_table.contains_key(&session.sid()) {
        return;
    }
    session_table.insert(session.sid(), &session);
}

/// Lists all processes.
pub fn processes() -> Vec<Arc<Process>> {
    PROCESS_TABLE.read().values().collect()
}

/// Finds the thread with the given TID.
pub fn get_thread(tid: Pid) -> LinuxResult<Arc<Thread>> {
    THREAD_TABLE.read().get(&tid).ok_or(LinuxError::ESRCH)
}

/// Finds the process with the given PID.
pub fn get_process(pid: Pid) -> LinuxResult<Arc<Process>> {
    PROCESS_TABLE.read().get(&pid).ok_or(LinuxError::ESRCH)
}

/// Finds the process group with the given PGID.
pub fn get_process_group(pgid: Pid) -> LinuxResult<Arc<ProcessGroup>> {
    PROCESS_GROUP_TABLE
        .read()
        .get(&pgid)
        .ok_or(LinuxError::ESRCH)
}

/// Finds the session with the given SID.
pub fn get_session(sid: Pid) -> LinuxResult<Arc<Session>> {
    SESSION_TABLE.read().get(&sid).ok_or(LinuxError::ESRCH)
}

/// Returns umask of the current process.
pub fn current_umask() -> u32 {
    StarryTaskExt::of(&current())
        .process_data()
        .umask
        .load(Ordering::SeqCst)
}
