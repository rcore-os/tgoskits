//! User task management.

mod cred;
mod futex;
mod ops;
mod resources;
mod signal;
mod stat;
mod timer;
mod user;

use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::{
    cell::RefCell,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering},
};

use ax_hal::time::TimeValue;
use ax_sync::{Mutex, spin::SpinNoIrq};
use ax_task::{TaskExt, TaskInner};
use axpoll::PollSet;
use extern_trait::extern_trait;
use scope_local::{ActiveScope, Scope};
use spin::RwLock;
use starry_process::Process;
use starry_signal::{
    Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};

pub use self::{cred::*, futex::*, ops::*, resources::*, signal::*, stat::*, timer::*, user::*};
use crate::mm::AddrSpace;

/// Size of the syscall instruction for the current architecture.
/// Used by SA_RESTART to back up the program counter.
#[cfg(target_arch = "x86_64")]
pub const SYSCALL_INSN_LEN: usize = 2;
/// Size of the syscall instruction for the current architecture.
/// Used by SA_RESTART to back up the program counter.
#[cfg(not(target_arch = "x86_64"))]
pub const SYSCALL_INSN_LEN: usize = 4;

///  A wrapper type that assumes the inner type is `Sync`.
#[repr(transparent)]
pub struct AssumeSync<T>(pub T);

unsafe impl<T> Sync for AssumeSync<T> {}

impl<T> Deref for AssumeSync<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The inner data of a thread.
pub struct Thread {
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
    pub signal: Arc<ThreadSignalManager>,

    /// Time manager
    ///
    /// This is assumed to be `Sync` because it's only borrowed mutably during
    /// context switches, which is exclusive to the current thread.
    pub time: AssumeSync<RefCell<TimeManager>>,

    /// The OOM score adjustment value.
    oom_score_adj: AtomicI32,

    /// Ready to exit
    pub exit: Arc<AtomicBool>,

    /// Indicates whether the thread is currently accessing user memory.
    accessing_user_memory: AtomicBool,

    /// Self exit event
    pub exit_event: Arc<PollSet>,

    /// The registered rseq area pointer (user address) for restartable
    /// sequences (`rseq(2)`).
    rseq_area: AtomicUsize,

    /// The signal to send to this thread when its parent dies (PR_SET_PDEATHSIG).
    pdeathsig: AtomicU32,

    /// Process credentials (uid, gid, etc.).
    cred: SpinNoIrq<Arc<Cred>>,
}

impl Thread {
    /// Create a new [`Thread`].
    ///
    /// If `parent_cred` is `Some`, the thread inherits the parent's credentials;
    /// otherwise it starts with root credentials (used for the init process).
    pub fn new(tid: u32, proc_data: Arc<ProcessData>, parent_cred: Option<Arc<Cred>>) -> Box<Self> {
        let cred = parent_cred.unwrap_or_else(|| Arc::new(Cred::root()));
        Box::new(Thread {
            signal: ThreadSignalManager::new(tid, proc_data.signal.clone()),
            proc_data,
            clear_child_tid: AtomicUsize::new(0),
            robust_list_head: AtomicUsize::new(0),
            time: AssumeSync(RefCell::new(TimeManager::new())),
            exit: Arc::new(AtomicBool::new(false)),
            oom_score_adj: AtomicI32::new(200),
            accessing_user_memory: AtomicBool::new(false),
            exit_event: Arc::default(),
            rseq_area: AtomicUsize::new(0),
            pdeathsig: AtomicU32::new(0),
            cred: SpinNoIrq::new(cred),
        })
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

    /// Get the oom score adjustment value.
    pub fn oom_score_adj(&self) -> i32 {
        self.oom_score_adj.load(Ordering::SeqCst)
    }

    /// Set the oom score adjustment value.
    pub fn set_oom_score_adj(&self, value: i32) {
        self.oom_score_adj.store(value, Ordering::SeqCst);
    }

    /// Check if the thread is ready to exit.
    pub fn pending_exit(&self) -> bool {
        self.exit.load(Ordering::Acquire)
    }

    /// Set the thread to exit.
    pub fn set_exit(&self) {
        self.exit.store(true, Ordering::Release);
    }

    /// Check if the thread is accessing user memory.
    pub fn is_accessing_user_memory(&self) -> bool {
        self.accessing_user_memory.load(Ordering::Acquire)
    }

    /// Set the accessing user memory flag.
    pub fn set_accessing_user_memory(&self, accessing: bool) {
        self.accessing_user_memory
            .store(accessing, Ordering::Release);
    }

    /// Get the pdeathsig value (signal sent to this thread when parent exits).
    pub fn pdeathsig(&self) -> u32 {
        self.pdeathsig.load(Ordering::Relaxed)
    }

    /// Set the pdeathsig value.
    pub fn set_pdeathsig(&self, sig: u32) {
        self.pdeathsig.store(sig, Ordering::Relaxed);
    }

    /// Get a snapshot of the current credentials (clones the `Arc`).
    pub fn cred(&self) -> Arc<Cred> {
        self.cred.lock().clone()
    }

    /// Replace the credentials with `new_cred` for this thread only.
    /// Prefer `set_cred` for credential-changing syscalls.
    fn set_cred_single(&self, new_cred: Arc<Cred>) {
        *self.cred.lock() = new_cred;
    }

    /// Replace the credentials for ALL threads in the same process.
    ///
    /// POSIX requires that credential changes (setuid, setresuid, etc.)
    /// affect all threads in a process. On Linux, the kernel stores
    /// credentials per-thread and the C library synchronizes via signals.
    /// musl's setxid synchronization does NOT work on StarryOS, so we
    /// implement this at the kernel level instead.
    ///
    /// Lock ordering: threads are updated in ascending TID order to
    /// prevent AB/BA deadlock when two threads call set_cred
    /// concurrently on SMP.
    pub fn set_cred(&self, new_cred: Cred) {
        let new_arc = Arc::new(new_cred);

        // Collect TIDs and sort to establish a consistent lock order.
        let mut tids = self.proc_data.proc.threads();
        tids.sort_unstable();

        for tid in &tids {
            if let Ok(task) = ops::get_task(*tid) {
                if let Some(thr) = task.try_as_thread() {
                    thr.set_cred_single(new_arc.clone());
                }
            }
        }
    }

    /// Get the registered rseq area pointer.
    pub fn rseq_area(&self) -> usize {
        self.rseq_area.load(Ordering::SeqCst)
    }

    /// Set the registered rseq area pointer.
    pub fn set_rseq_area(&self, addr: usize) {
        self.rseq_area.store(addr, Ordering::SeqCst);
    }
}

#[extern_trait]
impl TaskExt for Box<Thread> {
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
        self.task_ext()
            .map(|ext| ext.downcast_ref::<Box<Thread>>().as_ref())
    }
}

/// [`Process`]-shared data.
pub struct ProcessData {
    /// The process.
    pub proc: Arc<Process>,
    /// The executable path
    pub exe_path: RwLock<String>,
    /// The command line arguments
    pub cmdline: RwLock<Arc<Vec<String>>>,
    /// The virtual memory address space.
    // TODO: scopify
    pub aspace: Arc<Mutex<AddrSpace>>,
    /// The resource scope
    pub scope: RwLock<Scope>,
    /// The user heap top
    heap_top: AtomicUsize,

    /// The resource limits
    pub rlim: RwLock<Rlimits>,

    /// The child exit wait event
    pub child_exit_event: Arc<PollSet>,
    /// Self exit event
    pub exit_event: Arc<PollSet>,
    /// Woken every time a thread in this process exits. Used by a thread
    /// performing `execve` to wait for its siblings to be reaped.
    pub thread_exit_event: Arc<PollSet>,
    /// Serializes `execve` within the process. Only one thread can be tearing
    /// down the thread group at a time; concurrent attempts from other threads
    /// return `EINTR` (they are about to be killed by SIGKILL anyway).
    pub exec_lock: Mutex<()>,
    /// The exit signal of the thread
    pub exit_signal: Option<Signo>,

    /// The process signal manager
    pub signal: Arc<ProcessSignalManager>,

    /// The futex table.
    futex_table: Arc<FutexTable>,

    /// The default mask for file permissions.
    umask: AtomicU32,

    /// Accumulated CPU time of waited children (utime + stime).
    /// Updated when wait() reaps a child.
    children_cpu_time: SpinNoIrq<(TimeValue, TimeValue)>,
}

impl ProcessData {
    /// Create a new [`ProcessData`].
    pub fn new(
        proc: Arc<Process>,
        exe_path: String,
        cmdline: Arc<Vec<String>>,
        aspace: Arc<Mutex<AddrSpace>>,
        signal_actions: Arc<SpinNoIrq<SignalActions>>,
        exit_signal: Option<Signo>,
    ) -> Arc<Self> {
        Arc::new(Self {
            proc,
            exe_path: RwLock::new(exe_path),
            cmdline: RwLock::new(cmdline),
            aspace,
            scope: RwLock::new(Scope::new()),
            heap_top: AtomicUsize::new(crate::config::USER_HEAP_BASE),

            rlim: RwLock::default(),

            child_exit_event: Arc::default(),
            exit_event: Arc::default(),
            thread_exit_event: Arc::default(),
            exec_lock: Mutex::new(()),
            exit_signal,

            signal: Arc::new(ProcessSignalManager::new(
                signal_actions,
                crate::config::SIGNAL_TRAMPOLINE,
            )),

            futex_table: Arc::new(FutexTable::new()),

            umask: AtomicU32::new(0o022),

            children_cpu_time: SpinNoIrq::new((TimeValue::ZERO, TimeValue::ZERO)),
        })
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

    /// Get the accumulated CPU time of waited children.
    pub fn children_cpu_time(&self) -> (TimeValue, TimeValue) {
        *self.children_cpu_time.lock()
    }

    /// Accumulate a child's CPU time when it is reaped by wait().
    pub fn add_child_cpu_time(&self, utime: TimeValue, stime: TimeValue) {
        let mut time = self.children_cpu_time.lock();
        time.0 += utime;
        time.1 += stime;
    }
}
