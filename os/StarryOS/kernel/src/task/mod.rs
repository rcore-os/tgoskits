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
use ax_task::{TaskExt, TaskInner, WaitQueue};
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

/// A one-shot flag that suppresses exactly one signal check.
struct NextSignalCheckBlock(AtomicBool);

impl NextSignalCheckBlock {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn block(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn unblock(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
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

    /// Skips one signal check after returning from a user-space signal handler.
    block_next_signal_check: NextSignalCheckBlock,

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
            block_next_signal_check: NextSignalCheckBlock::new(),
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
            if let Ok(task) = ops::get_task(*tid)
                && let Some(thr) = task.try_as_thread()
            {
                thr.set_cred_single(new_arc.clone());
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

    /// Block the next signal check for this thread.
    pub fn block_next_signal_check(&self) {
        self.block_next_signal_check.block();
    }

    /// Consume and clear the one-shot signal-check block flag.
    pub fn unblock_next_signal_check(&self) -> bool {
        self.block_next_signal_check.unblock()
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

/// A one-shot completion for vfork synchronization.
///
/// This avoids lost-wakeup races by recording the "done" state under the same
/// lock as the wait queue. If the child completes before the parent enters the
/// wait, the parent will see `done == true` and skip waiting.
pub struct VforkDone {
    done: bool,
    wq: Arc<WaitQueue>,
}

impl VforkDone {
    pub fn new(wq: Arc<WaitQueue>) -> Self {
        Self { done: false, wq }
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
    aspace: SpinNoIrq<Arc<Mutex<AddrSpace>>>,
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
    /// The exit signal of the thread
    pub exit_signal: Option<Signo>,

    /// The process signal manager
    pub signal: Arc<ProcessSignalManager>,

    /// The futex table.
    futex_table: Arc<FutexTable>,

    /// If this process was created by vfork, this tracks completion state.
    /// The parent waits until `done` becomes true. Protected by the same lock
    /// as the wait queue to avoid lost wakeup races.
    vfork_done: SpinNoIrq<Option<VforkDone>>,

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
            aspace: SpinNoIrq::new(aspace),
            scope: RwLock::new(Scope::new()),
            heap_top: AtomicUsize::new(crate::config::USER_HEAP_BASE),

            rlim: RwLock::default(),

            child_exit_event: Arc::default(),
            exit_event: Arc::default(),
            exit_signal,

            signal: Arc::new(ProcessSignalManager::new(
                signal_actions,
                crate::config::SIGNAL_TRAMPOLINE,
            )),

            futex_table: Arc::new(FutexTable::new()),

            vfork_done: SpinNoIrq::new(None),

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

    /// Returns a clone of the address space Arc.
    pub fn aspace(&self) -> Arc<Mutex<AddrSpace>> {
        self.aspace.lock().clone()
    }

    /// Replace this process's address space with a new one.
    ///
    /// # Why `mem::replace` instead of `*guard = new_aspace`
    ///
    /// `self.aspace` is a `SpinNoIrq<Arc<Mutex<AddrSpace>>>`. Locking it
    /// disables IRQs and increments `preempt_count`, putting us in atomic
    /// context. A plain assignment (`*guard = new_aspace`) would drop the
    /// **old** `Arc<Mutex<AddrSpace>>` while the `SpinNoIrq` guard is still
    /// alive. If that was the last strong reference (e.g. after a
    /// `CLONE_VM` + `execve`), the destructor chain would be:
    ///
    /// ```text
    /// Arc::drop → Mutex<AddrSpace>::drop → AddrSpace::drop
    ///   → self.clear() → areas.clear() → FileBackendInner::drop
    ///     → cache.remove_evict_listener()
    ///       → evict_listeners.lock()        ← sleeping Mutex
    ///         → might_sleep()               ← PANIC (atomic context)
    /// ```
    ///
    /// `mem::replace` moves the old Arc out of the guard so it is dropped
    /// **after** the `SpinNoIrq` guard, in normal preemptible context.
    pub fn replace_aspace(&self, new_aspace: Arc<Mutex<AddrSpace>>) {
        let _old = {
            let mut guard = self.aspace.lock();
            core::mem::replace(&mut *guard, new_aspace)
        };
        // `_old` drops here — SpinNoIrq already released, IRQs re-enabled.
    }

    /// Set the vfork completion (called on the child after a vfork,
    /// before the child task is spawned).
    pub fn set_vfork_done(&self, wq: Arc<WaitQueue>) {
        *self.vfork_done.lock() = Some(VforkDone::new(wq));
    }

    /// Wait for vfork completion. Returns immediately if already done.
    /// This should be called by the parent after spawning the vfork child.
    pub fn wait_vfork_done(&self) {
        let wq = {
            let guard = self.vfork_done.lock();
            match guard.as_ref() {
                Some(vfork) => vfork.wq.clone(),
                None => return, // No vfork, shouldn't happen but be safe.
            }
        };
        // Wait until done. The condition is checked under lock in wait_until.
        wq.wait_until(|| {
            self.vfork_done
                .lock()
                .as_ref()
                .map(|v| v.done)
                .unwrap_or(true)
        });
    }

    /// Notify the vfork parent that this child has exec'd or exited.
    /// No-op if this process was not created by vfork.
    pub fn notify_vfork_done(&self) {
        // Set done under the lock, then drop the lock before notifying
        // to avoid lock-order inversion with the wait-queue internal lock.
        let wq = {
            let mut guard = self.vfork_done.lock();
            match guard.as_mut() {
                Some(vfork) => {
                    vfork.done = true;
                    vfork.wq.clone()
                }
                None => return,
            }
            // guard dropped here
        };
        wq.notify_one(true);
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::NextSignalCheckBlock;

    #[test]
    fn old_global_signal_check_block_leaks_between_threads() {
        static OLD_BLOCK_NEXT_SIGNAL_CHECK: AtomicBool = AtomicBool::new(false);

        fn block_next_signal() {
            OLD_BLOCK_NEXT_SIGNAL_CHECK.store(true, Ordering::SeqCst);
        }

        fn unblock_next_signal() -> bool {
            OLD_BLOCK_NEXT_SIGNAL_CHECK.swap(false, Ordering::SeqCst)
        }

        // Simulate thread A returning from `rt_sigreturn()`.
        block_next_signal();

        // Simulate thread B reaching the user return path first and incorrectly
        // consuming thread A's one-shot state.
        assert!(
            unblock_next_signal(),
            "the old global flag leaks across logical threads"
        );
        assert!(!unblock_next_signal());
    }

    #[test]
    fn per_thread_signal_check_block_is_isolated() {
        let thread_a = NextSignalCheckBlock::new();
        let thread_b = NextSignalCheckBlock::new();

        thread_a.block();

        assert!(
            !thread_b.unblock(),
            "thread B must not observe thread A's signal-check block"
        );
        assert!(thread_a.unblock());
        assert!(!thread_a.unblock());
    }
}
