//! User task management.

mod stat;

use alloc::{
    boxed::Box,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    cell::RefCell,
    ops::Deref,
    sync::atomic::{AtomicI32, AtomicU32, AtomicUsize, Ordering},
};

use axerrno::{LinuxError, LinuxResult};
use axhal::context::UspaceContext;
use axmm::AddrSpace;
use axprocess::{Pid, Process, ProcessGroup, Session};
use axsignal::{
    SignalInfo, Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};
use axsync::{Mutex, spin::SpinNoIrq};
use axtask::{AxTaskRef, TaskExt, TaskInner, WeakAxTaskRef, current};
use event_listener::Event;
use extern_trait::extern_trait;
use lazy_static::lazy_static;
use linux_raw_sys::general::SI_KERNEL;
use scope_local::{ActiveScope, Scope};
use spin::RwLock;
use weak_map::WeakMap;

pub use self::stat::TaskStat;
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

/// The inner data of a thread.
pub struct ThreadInner {
    /// The process data shared by all threads in the process.
    pub proc_data: Arc<ProcessData>,

    /// The clear thread tid field
    ///
    /// See <https://manpages.debian.org/unstable/manpages-dev/set_tid_address.2.en.html#clear_child_tid>
    ///
    /// When the thread exits, the kernel clears the word at this address if it
    /// is not NULL.
    clear_child_tid: AtomicUsize,

    /// The head of the robust list
    robust_list_head: AtomicUsize,

    /// The thread-level signal manager
    pub signal: ThreadSignalManager,

    /// Time manager
    ///
    /// This is assumed to be `Sync` because it's only borrowed mutably during
    /// context switches, which is exclusive to the current thread.
    pub time: AssumeSync<RefCell<TimeManager>>,

    /// The bitset used for futex operations (FUTEX_{WAIT,WAKE}_BITSET).
    futex_bitset: AtomicU32,

    /// The OOM score adjustment value.
    oom_score_adj: AtomicI32,
}

impl ThreadInner {
    /// Create a new [`ThreadInner`].
    pub fn new(proc_data: Arc<ProcessData>) -> Self {
        ThreadInner {
            signal: ThreadSignalManager::new(proc_data.signal.clone()),
            proc_data,
            clear_child_tid: AtomicUsize::new(0),
            robust_list_head: AtomicUsize::new(0),
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

    /// Get the robust list head.
    pub fn robust_list_head(&self) -> usize {
        self.robust_list_head.load(Ordering::SeqCst)
    }

    /// Set the robust list head.
    pub fn set_robust_list_head(&self, robust_list_head: usize) {
        self.robust_list_head
            .store(robust_list_head, Ordering::SeqCst);
    }

    /// Get the futex bitset.
    pub fn futex_bitset(&self) -> u32 {
        self.futex_bitset.load(Ordering::SeqCst)
    }

    /// Set the futex bitset.
    pub fn set_futex_bitset(&self, bitset: u32) {
        self.futex_bitset.store(bitset, Ordering::SeqCst);
    }

    /// Get the oom score adjustment value.
    pub fn oom_score_adj(&self) -> i32 {
        self.oom_score_adj.load(Ordering::SeqCst)
    }

    /// Set the oom score adjustment value.
    pub fn set_oom_score_adj(&self, value: i32) {
        self.oom_score_adj.store(value, Ordering::SeqCst);
    }
}

/// Extended thread data for the monolithic kernel.
pub struct Thread(Box<ThreadInner>);

impl Deref for Thread {
    type Target = ThreadInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[extern_trait]
unsafe impl TaskExt for Thread {
    fn on_enter(&self) {
        let scope = self.proc_data.scope.read();
        unsafe { ActiveScope::set(&scope) };
        core::mem::forget(scope);
    }

    fn on_leave(&self) {
        ActiveScope::set_global();
        unsafe { self.proc_data.scope.force_read_decrement() };
    }
}

/// Helper trait to access the thread from a task.
pub trait AsThread {
    /// Try to get the thread from the task.
    fn try_as_thread(&self) -> Option<&Thread>;

    /// Get the thread from the task, panicking if it is a kernel task.
    fn as_thread(&self) -> &Thread {
        self.try_as_thread().expect("kernel task")
    }
}

impl AsThread for TaskInner {
    fn try_as_thread(&self) -> Option<&Thread> {
        self.task_ext().map(|ext| unsafe { ext.downcast_ref() })
    }
}

impl Thread {
    /// Create a new [`Thread`].
    pub fn new(proc_data: Arc<ProcessData>) -> Self {
        Self(Box::new(ThreadInner::new(proc_data)))
    }
}

/// [`Process`]-shared data.
pub struct ProcessData {
    /// The process.
    pub proc: Arc<Process>,
    /// The executable path
    pub exe_path: RwLock<String>,
    /// The virtual memory address space.
    // TODO: scopify
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
    ///
    /// Reasons for using [`SpinNoIrqRawMutex`]: we may send signal during IRQs,
    /// and thus we need to prevent IRQ from happening when the lock is held.
    pub signal: Arc<ProcessSignalManager>,

    /// The futex table.
    futex_table: FutexTable,

    /// The default mask for file permissions.
    umask: AtomicU32,
}

impl ProcessData {
    /// Create a new [`ProcessData`].
    pub fn new(
        proc: Arc<Process>,
        exe_path: String,
        aspace: Arc<Mutex<AddrSpace>>,
        signal_actions: Arc<SpinNoIrq<SignalActions>>,
        exit_signal: Option<Signo>,
    ) -> Arc<Self> {
        Arc::new(Self {
            proc,
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
        })
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

    /// Get the umask.
    pub fn umask(&self) -> u32 {
        self.umask.load(Ordering::SeqCst)
    }

    /// Set the umask.
    pub fn set_umask(&self, umask: u32) {
        self.umask.store(umask, Ordering::SeqCst);
    }

    /// Set the umask and return the old value.
    pub fn replace_umask(&self, umask: u32) -> u32 {
        self.umask.swap(umask, Ordering::SeqCst)
    }
}

lazy_static! {
    static ref SHARED_FUTEX_TABLE: FutexTable = FutexTable::new();
}

static TASK_TABLE: RwLock<WeakMap<Pid, WeakAxTaskRef>> = RwLock::new(WeakMap::new());

static PROCESS_TABLE: RwLock<WeakMap<Pid, Weak<ProcessData>>> = RwLock::new(WeakMap::new());

static PROCESS_GROUP_TABLE: RwLock<WeakMap<Pid, Weak<ProcessGroup>>> = RwLock::new(WeakMap::new());

static SESSION_TABLE: RwLock<WeakMap<Pid, Weak<Session>>> = RwLock::new(WeakMap::new());

/// Cleanup expired entries in the task tables.
///
/// This function is intended to be used during memory leak analysis to remove
/// possible noise caused by expired entries in the [`WeakMap`].
#[cfg(feature = "track")]
pub(crate) fn cleanup_task_tables() {
    TASK_TABLE.write().cleanup();
    PROCESS_TABLE.write().cleanup();
    PROCESS_GROUP_TABLE.write().cleanup();
    SESSION_TABLE.write().cleanup();
}

/// Add the task, the thread and possibly its process, process group and session
/// to the corresponding tables.
pub fn add_task_to_table(task: &AxTaskRef) {
    let tid = task.id().as_u64() as Pid;

    let mut task_table = TASK_TABLE.write();
    task_table.insert(tid, task);

    let proc_data = &task.as_thread().proc_data;
    let proc = &proc_data.proc;
    let pid = proc.pid();
    let mut proc_table = PROCESS_TABLE.write();
    if proc_table.contains_key(&pid) {
        return;
    }
    proc_table.insert(pid, proc_data);

    let pg = proc.group();
    let mut pg_table = PROCESS_GROUP_TABLE.write();
    if pg_table.contains_key(&pg.pgid()) {
        return;
    }
    pg_table.insert(pg.pgid(), &pg);

    let session = pg.session();
    let mut session_table = SESSION_TABLE.write();
    if session_table.contains_key(&session.sid()) {
        return;
    }
    session_table.insert(session.sid(), &session);
}

/// Lists all tasks.
pub fn tasks() -> Vec<AxTaskRef> {
    TASK_TABLE.read().values().collect()
}

/// Finds the task with the given TID.
pub fn get_task(tid: Pid) -> LinuxResult<AxTaskRef> {
    if tid == 0 {
        return Ok(current().clone());
    }
    TASK_TABLE.read().get(&tid).ok_or(LinuxError::ESRCH)
}

/// Lists all processes.
pub fn processes() -> Vec<Arc<ProcessData>> {
    PROCESS_TABLE.read().values().collect()
}

/// Finds the process with the given PID.
pub fn get_process_data(pid: Pid) -> LinuxResult<Arc<ProcessData>> {
    if pid == 0 {
        return Ok(current().as_thread().proc_data.clone());
    }
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

/// Poll the timer
pub fn poll_timer(task: &TaskInner) {
    let Some(thr) = task.try_as_thread() else {
        return;
    };
    let Ok(mut time) = thr.time.try_borrow_mut() else {
        // reentrant borrow, likely IRQ
        return;
    };
    time.poll(|signo| {
        thr.signal
            .send_signal(SignalInfo::new(signo, SI_KERNEL as _));
        task.set_interrupted(true);
    });
}

/// Sets the timer state.
pub fn set_timer_state(task: &TaskInner, state: TimerState) {
    let Some(thr) = task.try_as_thread() else {
        return;
    };
    let Ok(mut time) = thr.time.try_borrow_mut() else {
        // reentrant borrow, likely IRQ
        return;
    };
    time.poll(|signo| {
        thr.signal
            .send_signal(SignalInfo::new(signo, SI_KERNEL as _));
        task.set_interrupted(true);
    });
    time.set_state(state);
}
