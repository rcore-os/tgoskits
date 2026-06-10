//! User task management.

mod cred;
pub mod futex;
mod ops;
pub mod posix_timer;
mod resources;
mod signal;
mod stat;
mod timer;
mod user;

use alloc::{boxed::Box, collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::{
    cell::RefCell,
    ops::Deref,
    sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicUsize, Ordering},
};

use ax_runtime::hal::{cpu::uspace::UserContext, time::TimeValue};
use ax_sync::{Mutex, spin::SpinNoIrq};
use ax_task::{TaskExt, TaskInner};
use axpoll::PollSet;
use extern_trait::extern_trait;
use kernel_elf_parser::AuxEntry;
use scope_local::{ActiveScope, Scope};
use spin::RwLock;
use starry_process::Process;
use starry_signal::{
    SignalInfo, SignalSet, Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};

pub use self::{
    cred::*, futex::*, ops::*, posix_timer::PosixTimerTable, resources::*, signal::*, stat::*,
    timer::*, user::*,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyscallTraceState {
    #[default]
    None,
    Entry,
    Exit,
}

struct PtraceStopRecord {
    signo: Option<Signo>,
    uctx: UserContext,
    siginfo: Option<SignalInfo>,
    is_syscall: bool,
    reported: bool,
    event: u32,
    event_msg: usize,
}

struct PtracePendingEvent {
    event: u32,
    msg: usize,
}
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
    /// User-visible thread ID (the `Pid` returned by `gettid`).
    ///
    /// Initially equal to the underlying scheduler `TaskInner::id()`. The two
    /// diverge after a successful non-leader `execve`: Linux's `de_thread`
    /// step transfers the leader's TID/TGID to the calling thread so that
    /// `gettid() == getpid()` holds in the new image. We model that by
    /// updating this field while leaving the immutable scheduler ID alone.
    /// All user-facing TID lookups (`sys_gettid`, `set_tid_address`, signal
    /// child registration, `do_exit`'s thread-group bookkeeping, etc.) read
    /// this rather than the scheduler ID.
    tid: AtomicU32,

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

    /// Set by `sys_execve` when reaping sibling threads. The signal-check
    /// path turns this into a thread-only `do_exit(0, false)` — no group
    /// exit, no fatal-signal cascade — so the new image is left intact.
    exit_request: AtomicBool,

    /// The registered rseq area pointer (user address) for restartable
    /// sequences (`rseq(2)`).
    rseq_area: AtomicUsize,

    /// The rseq signature recorded at registration time.
    rseq_signature: AtomicU32,

    /// The signal to send to this thread when its parent dies (PR_SET_PDEATHSIG).
    pdeathsig: AtomicU32,

    /// PR_SET_NO_NEW_PRIVS: once set, cannot be unset.
    no_new_privs: AtomicBool,

    /// Process credentials (uid, gid, etc.).
    cred: SpinNoIrq<Arc<Cred>>,

    /// Signo (as u8) of the synchronous user-mode fault that
    /// [`raise_signal_fatal`] last force-delivered to this thread, or 0
    /// for "no fault dump owed". [`check_signals`] only emits the
    /// register dump when the signal it is about to terminate on
    /// matches this signo — otherwise a low-numbered pending signal
    /// (e.g. an external SIGTERM that landed before the SIGSEGV from a
    /// page fault) would consume the flag and either dump for the
    /// wrong context or, if it had a user handler, swallow the dump so
    /// the real fault terminated silently.
    pub fault_dump_signo: AtomicU8,

    pub kretprobe_stack: SpinNoIrq<alloc::vec::Vec<kprobe::retprobe::RetprobeInstance>>,

    /// Whether uid_map has been written for this thread's user namespace.
    uid_map_written: AtomicBool,

    /// Whether gid_map has been written for this thread's user namespace.
    gid_map_written: AtomicBool,

    /// Whether setgroups has been set to "deny" for this thread's user namespace.
    setgroups_deny: AtomicBool,
}

impl Thread {
    /// Create a new [`Thread`].
    ///
    /// If `parent_cred` is `Some`, the thread inherits the parent's credentials;
    /// otherwise it starts with root credentials (used for the init process).
    pub fn new(
        tid: u32,
        proc_data: Arc<ProcessData>,
        parent_cred: Option<Arc<Cred>>,
        signal_mask: SignalSet,
    ) -> Box<Self> {
        let cred = parent_cred.unwrap_or_else(|| Arc::new(Cred::root()));
        Box::new(Thread {
            tid: AtomicU32::new(tid),
            signal: ThreadSignalManager::new_with_blocked(
                tid,
                proc_data.signal.clone(),
                signal_mask,
            ),
            proc_data,
            clear_child_tid: AtomicUsize::new(0),
            robust_list_head: AtomicUsize::new(0),
            time: AssumeSync(RefCell::new(TimeManager::new())),
            exit: Arc::new(AtomicBool::new(false)),
            oom_score_adj: AtomicI32::new(200),
            accessing_user_memory: AtomicBool::new(false),
            block_next_signal_check: NextSignalCheckBlock::new(),
            exit_event: Arc::default(),
            exit_request: AtomicBool::new(false),
            rseq_area: AtomicUsize::new(0),
            rseq_signature: AtomicU32::new(0),
            pdeathsig: AtomicU32::new(0),
            no_new_privs: AtomicBool::new(false),
            cred: SpinNoIrq::new(cred),

            fault_dump_signo: AtomicU8::new(0),
            kretprobe_stack: SpinNoIrq::new(alloc::vec::Vec::new()),

            uid_map_written: AtomicBool::new(false),
            gid_map_written: AtomicBool::new(false),
            setgroups_deny: AtomicBool::new(false),
        })
    }

    /// Returns the user-visible TID for this thread.
    ///
    /// See the field doc on [`Thread::tid`] for why this can differ from
    /// the underlying scheduler `TaskInner::id()`.
    pub fn tid(&self) -> u32 {
        self.tid.load(Ordering::Acquire)
    }

    /// Updates the user-visible TID. Called only by `execve`'s de_thread
    /// step to transfer the leader's TID to a non-leader caller.
    pub(crate) fn set_tid(&self, tid: u32) {
        self.tid.store(tid, Ordering::Release);
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

    /// Consume a pending thread-only exit request, returning whether one
    /// was set. The flag is cleared in the same atomic step so that a
    /// re-entrant `check_signals` (the user loop drains signals in a
    /// while-loop) doesn't fire `do_exit` twice for the same zap.
    pub fn take_exit_request(&self) -> bool {
        self.exit_request.swap(false, Ordering::AcqRel)
    }

    /// Non-consuming probe for a pending thread-only exit request. Used
    /// by in-kernel wait loops that want to abort cooperatively without
    /// stealing the flag from the user-return `check_signals` path.
    pub fn has_exit_request(&self) -> bool {
        self.exit_request.load(Ordering::Acquire)
    }

    /// Request a thread-only exit. Honored by `check_signals` on the next
    /// return to user space, where it routes to `do_exit(0, false)`.
    pub fn set_exit_request(&self) {
        self.exit_request.store(true, Ordering::Release);
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

    /// Get the no_new_privs flag.
    pub fn no_new_privs(&self) -> bool {
        self.no_new_privs.load(Ordering::Relaxed)
    }

    /// Set the no_new_privs flag (one-way: once set, cannot be unset).
    pub fn set_no_new_privs(&self) {
        self.no_new_privs.store(true, Ordering::Relaxed);
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

    /// Get the registered rseq signature.
    pub fn rseq_signature(&self) -> u32 {
        self.rseq_signature.load(Ordering::SeqCst)
    }

    /// Check if uid_map has been written for this thread's user namespace.
    pub fn uid_map_written(&self) -> bool {
        self.uid_map_written.load(Ordering::Relaxed)
    }

    /// Mark uid_map as written.
    pub fn set_uid_map_written(&self, val: bool) {
        self.uid_map_written.store(val, Ordering::Relaxed);
    }

    /// Check if gid_map has been written for this thread's user namespace.
    pub fn gid_map_written(&self) -> bool {
        self.gid_map_written.load(Ordering::Relaxed)
    }

    /// Mark gid_map as written.
    pub fn set_gid_map_written(&self, val: bool) {
        self.gid_map_written.store(val, Ordering::Relaxed);
    }

    /// Check if setgroups has been set to "deny".
    pub fn setgroups_deny(&self) -> bool {
        self.setgroups_deny.load(Ordering::Relaxed)
    }

    /// Set the setgroups deny flag.
    pub fn set_setgroups_deny(&self, val: bool) {
        self.setgroups_deny.store(val, Ordering::Relaxed);
    }

    /// Set the registered rseq area pointer.
    pub fn set_rseq_area(&self, addr: usize) {
        self.rseq_area.store(addr, Ordering::SeqCst);
    }

    /// Set the registered rseq area pointer and signature.
    pub fn set_rseq_state(&self, addr: usize, sig: u32) {
        self.rseq_area.store(addr, Ordering::SeqCst);
        self.rseq_signature.store(sig, Ordering::SeqCst);
    }

    /// Clear the registered rseq state.
    pub fn clear_rseq_state(&self) {
        self.rseq_area.store(0, Ordering::SeqCst);
        self.rseq_signature.store(0, Ordering::SeqCst);
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
    #[track_caller]
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
/// lock as the waker set. If the child completes before the parent enters the
/// wait, the parent will see `done == true` and skip waiting.
///
/// We use [`PollSet`] (not `WaitQueue`) so the parent's wait can run inside
/// `block_on(interruptible(...))`: a sibling thread that does `execve` will
/// zap us via `task.interrupt()`, which only wakes futures-based polls, not
/// `WaitQueue::wait_until`. Without this, the execve initiator would deadlock
/// in its sibling-teardown loop waiting for us to exit.
pub struct VforkDone {
    done: bool,
    poll: Arc<PollSet>,
}

impl VforkDone {
    pub fn new(poll: Arc<PollSet>) -> Self {
        Self { done: false, poll }
    }
}

/// A pending job-control status change awaiting report to the parent's
/// `waitpid(WUNTRACED | WCONTINUED)`.
#[derive(Clone, Copy)]
pub enum JobStatus {
    /// The process stopped after receiving the given job-control signal
    /// (`SIGSTOP`/`SIGTSTP`/`SIGTTIN`/`SIGTTOU`).
    Stopped(Signo),
    /// The process continued after receiving `SIGCONT`.
    Continued,
}

/// Job-control state for a process, kept under a single lock so the stop flag
/// and the pending parent report are updated atomically (a concurrent
/// stop/continue on another CPU must not split the two).
///
/// `stopped` and `status` are **intentionally independent** and may legitimately
/// diverge — do not collapse them into one field. `stopped` is the live parked
/// state (cleared only by continue/kill); `status` is a one-shot report the
/// parent's `waitpid` consumes (so `stopped == Some` with `status == None` is
/// valid once the report has been reaped).
#[derive(Default)]
struct JobControl {
    /// `None` = running, `Some(signo)` = stopped by the given job-control
    /// signal. A stopped process parks its threads in the kernel until
    /// `SIGCONT` (or `SIGKILL`) is delivered.
    stopped: Option<Signo>,
    /// Pending status change for the parent's `waitpid`, consumed once
    /// reported. Single-slot: a new stop/continue before the parent reaps the
    /// previous one overwrites it (unlike Linux, which queues each SIGCHLD).
    /// Adequate for the single-threaded job-control this targets.
    status: Option<JobStatus>,
    /// Bumped on every continue. A thread about to park (`set_job_stopped`)
    /// snapshots this; if it changed by the time the thread checks before
    /// parking, a `SIGCONT` raced in after the stop was recorded and the park
    /// is skipped. This closes the STOP-immediately-followed-by-CONT race
    /// (e.g. busybox `killall5 -STOP` then `-CONT`) without having to scrub the
    /// pending-signal queue.
    continue_generation: u64,
}

/// [`Process`]-shared data.
pub struct ProcessImage {
    pub exe_path: String,
    pub cmdline: Arc<Vec<String>>,
    pub auxv: Vec<AuxEntry>,
}

impl ProcessImage {
    pub fn new(exe_path: String, cmdline: Arc<Vec<String>>, auxv: Vec<AuxEntry>) -> Self {
        Self {
            exe_path,
            cmdline,
            auxv,
        }
    }
}

pub struct ProcessData {
    /// The process.
    pub proc: Arc<Process>,
    /// The executable path
    pub exe_path: RwLock<String>,
    /// The command line arguments
    pub cmdline: RwLock<Arc<Vec<String>>>,
    /// Auxiliary vector entries exported via `/proc/[pid]/auxv`.
    pub auxv: RwLock<Vec<AuxEntry>>,
    /// The virtual memory address space.
    // TODO: scopify
    aspace: SpinNoIrq<Arc<Mutex<AddrSpace>>>,
    /// Per-process uprobe manager. Uprobes plant an `int3` in *this* process'
    /// user text, so (unlike the global kprobe manager) the registry is
    /// per-address-space. A *sleeping* mutex, because arming/disarming
    /// manipulates the user address space (page-table query, faulting reads,
    /// mapping the out-of-line single-step page) which requires sleeping locks;
    /// the exception-context breakpoint/debug handlers acquire it with
    /// `try_lock()` (a single CAS, safe in atomic context) instead.
    pub uprobe_manager: crate::kprobe::KprobeManager,
    /// Per-process uprobe point list, paired with [`Self::uprobe_manager`].
    pub uprobe_point_list: Mutex<crate::kprobe::KprobePointList>,
    /// The resource scope
    pub scope: RwLock<Scope>,
    /// The namespace proxy — aggregates all namespace types for this process.
    pub nsproxy: SpinNoIrq<axnsproxy::NsProxy>,
    /// The user heap top
    heap_top: AtomicUsize,

    /// The resource limits
    pub rlim: RwLock<Rlimits>,

    /// The child exit wait event
    pub child_exit_event: Arc<PollSet>,
    /// Self exit event
    pub exit_event: Arc<PollSet>,
    /// Woken every time a thread in this process exits. Used by a thread
    /// performing `execve` to wait for siblings to be reaped.
    pub thread_exit_event: Arc<PollSet>,
    /// Serializes `execve` within the process. Only one thread can be
    /// tearing down the thread group at a time; concurrent attempts return
    /// `EINTR` (the loser is about to be zapped anyway).
    pub exec_lock: Mutex<()>,
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

    /// The process nice value used by getpriority/setpriority compatibility.
    nice: AtomicI32,

    /// Process-local membarrier(2) registration state bitmask.
    membarrier_state: AtomicU32,

    /// PR_GET_DUMPABLE / PR_SET_DUMPABLE value (default 1 = SUID_DUMP_USER).
    /// Cleared to 0 (SUID_DUMP_DISABLE) whenever the effective UID/GID
    /// changes via setuid/setresuid/setreuid (man 2 setuid §NOTES:
    /// "If uid is different from the old effective UID, the process will
    /// be forbidden from leaving core dumps").
    /// Linux stores this on `mm_struct`; StarryOS keeps it process-wide.
    dumpable: AtomicI32,

    /// PR_GET_THP_DISABLE / PR_SET_THP_DISABLE value.
    /// StarryOS does not implement transparent huge pages, but userspace may
    /// set this as a compatibility hint and later query it.
    thp_disable: AtomicU32,

    /// Accumulated CPU time of waited children (utime + stime).
    /// Updated when wait() reaps a child.
    children_cpu_time: SpinNoIrq<(TimeValue, TimeValue)>,

    /// Pid of the process currently tracing this process, if any.
    ptrace_tracer_pid: AtomicU32,

    /// Set by `ptrace(PTRACE_TRACEME)` to let the parent observe debugger-style
    /// stops from this process.
    ptrace_traceme: AtomicBool,

    /// Current ptrace stop records, keyed by stopped TID.
    ptrace_stop: SpinNoIrq<BTreeMap<u32, PtraceStopRecord>>,

    /// TID selected by the most recent ptrace request.
    ptrace_stop_tid: AtomicU32,

    /// Wakes a traced task that is sleeping in a ptrace stop.
    ptrace_stop_event: Arc<PollSet>,

    /// Signal number to deliver on resume, keyed by resumed TID.
    /// 0 means suppress the signal; non-zero means deliver that signal.
    ptrace_resume_signo: SpinNoIrq<BTreeMap<u32, u32>>,

    /// One-shot signal number that came from ptrace resume injection.
    /// The signal subsystem still handles disposition and handlers, but the
    /// next matching signal delivery must not stop for ptrace again.
    ptrace_resume_signal_bypass: SpinNoIrq<BTreeMap<u32, u32>>,

    /// Set by `execve` when the calling thread was `PTRACE_TRACEME`.
    /// Cleared after the exec-stop is delivered in the user-return loop.
    ptrace_exec_stop_pending: AtomicBool,

    /// Set by `PTRACE_ATTACH` / `PTRACE_SEIZE`.
    ptrace_attached: AtomicBool,

    /// TID selected by `PTRACE_SINGLESTEP`; causes a temporary EBREAK insertion.
    ptrace_singlestep_tid: AtomicU32,

    /// Set by `PTRACE_SYSCALL`; causes syscall-entry/exit stops, keyed by TID.
    ptrace_syscall_trace: SpinNoIrq<BTreeMap<u32, SyscallTraceState>>,

    /// Bitmask of PTRACE_O_* options set via `PTRACE_SETOPTIONS`.
    ptrace_options: AtomicUsize,

    /// Pending ptrace events that have not yet been bound to their owner TID stops.
    ptrace_pending_event: SpinNoIrq<BTreeMap<u32, PtracePendingEvent>>,

    /// Saved instruction overwritten by single-step EBREAK, keyed by TID.
    ptrace_ss_saved_insn: SpinNoIrq<BTreeMap<u32, (usize, usize)>>,

    /// FP register snapshot captured when entering ptrace stop, keyed by TID.
    /// Stored as raw bytes to avoid arch-specific crate dependency.
    ptrace_stop_fp_data: SpinNoIrq<BTreeMap<u32, ([u64; 32], usize)>>,

    /// Linux process personality flags. Starry does not randomize userspace
    /// mappings yet, but debuggers still probe and set ADDR_NO_RANDOMIZE.
    personality: AtomicUsize,

    /// POSIX per-process interval timers (timer_create/timer_settime/etc.)
    pub posix_timers: Arc<PosixTimerTable>,

    /// `true` when this process shares its [`AddrSpace`] with a parent/sibling
    /// (`CLONE_VM`, e.g. vfork / posix_spawn). In that case the last thread must
    /// **not** clear the address space on exit — the co-owner may still be
    /// running.
    ///
    /// `false` for normal `fork()` children and after a successful `execve`
    /// installs a private address space.
    vm_aspace_shared: AtomicBool,

    /// Set after [`Self::release_aspace_slot_if_needed`] runs so `Drop` does not
    /// double-decrement [`AddrSpace::process_slots`].
    aspace_slot_released: AtomicBool,

    /// Job-control state (stop flag + pending parent report) under one lock.
    job_control: SpinNoIrq<JobControl>,

    /// Woken to release threads parked in a job-control stop. Fired by
    /// `SIGCONT` (continue) and `SIGKILL` (force-resume so the kill proceeds).
    cont_event: Arc<PollSet>,
}

impl ProcessData {
    /// Create a new [`ProcessData`].
    pub fn new(
        proc: Arc<Process>,
        image: ProcessImage,
        aspace: Arc<Mutex<AddrSpace>>,
        signal_actions: Arc<SpinNoIrq<SignalActions>>,
        exit_signal: Option<Signo>,
        vm_aspace_shared: bool,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            proc,
            exe_path: RwLock::new(image.exe_path),
            cmdline: RwLock::new(image.cmdline),
            auxv: RwLock::new(image.auxv),
            aspace: SpinNoIrq::new(aspace),
            uprobe_manager: crate::kprobe::KprobeManager::new(),
            uprobe_point_list: Mutex::new(crate::kprobe::KprobePointList::new()),
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

            nsproxy: SpinNoIrq::new(axnsproxy::NsProxy::new_root()),

            vfork_done: SpinNoIrq::new(None),

            umask: AtomicU32::new(0o022),
            nice: AtomicI32::new(0),
            membarrier_state: AtomicU32::new(0),
            dumpable: AtomicI32::new(1),
            thp_disable: AtomicU32::new(0),

            children_cpu_time: SpinNoIrq::new((TimeValue::ZERO, TimeValue::ZERO)),

            ptrace_tracer_pid: AtomicU32::new(0),
            ptrace_traceme: AtomicBool::new(false),
            ptrace_stop: SpinNoIrq::new(BTreeMap::new()),
            ptrace_stop_tid: AtomicU32::new(0),
            ptrace_stop_event: Arc::default(),
            ptrace_resume_signo: SpinNoIrq::new(BTreeMap::new()),
            ptrace_resume_signal_bypass: SpinNoIrq::new(BTreeMap::new()),
            ptrace_exec_stop_pending: AtomicBool::new(false),
            ptrace_attached: AtomicBool::new(false),
            ptrace_singlestep_tid: AtomicU32::new(0),
            ptrace_syscall_trace: SpinNoIrq::new(BTreeMap::new()),
            ptrace_options: AtomicUsize::new(0),
            ptrace_pending_event: SpinNoIrq::new(BTreeMap::new()),
            ptrace_ss_saved_insn: SpinNoIrq::new(BTreeMap::new()),
            ptrace_stop_fp_data: SpinNoIrq::new(BTreeMap::new()),

            personality: AtomicUsize::new(0),

            posix_timers: Arc::new(PosixTimerTable::default()),

            vm_aspace_shared: AtomicBool::new(vm_aspace_shared),
            aspace_slot_released: AtomicBool::new(false),

            job_control: SpinNoIrq::new(JobControl::default()),
            cont_event: Arc::default(),
        });
        // Clone the Arc in a separate statement: a temporary `SpinNoIrq` guard
        // from `lock()` lives until the end of the statement, so calling
        // `attach_process_slot` (which locks `Mutex<AddrSpace>`) in the same
        // expression would nest a sleepable lock inside atomic context.
        let aspace_arc = this.aspace.lock().clone();
        crate::mm::attach_process_slot(&aspace_arc);
        this
    }

    /// Whether this process shares its VM address space (`CLONE_VM`).
    #[inline]
    pub fn vm_aspace_shared(&self) -> bool {
        self.vm_aspace_shared.load(Ordering::Acquire)
    }

    /// Called after `execve` commits a fresh private address space so exit
    /// teardown may clear VMAs without touching a vfork parent's mappings.
    #[inline]
    pub fn mark_vm_aspace_private_after_exec(&self) {
        self.vm_aspace_shared.store(false, Ordering::Release);
    }

    /// Release this process's [`AddrSpace::process_slots`] entry.
    ///
    /// Invoked from the last-thread exit path so inode-scoped accounting (memfd
    /// shared-writable counts, etc.) is torn down before `waitpid` returns, and
    /// again from `Drop` if not already run. Uses reference counting: only the
    /// last slot holder triggers [`AddrSpace::clear`], so `CLONE_VM` co-owners
    /// are unaffected.
    pub fn release_aspace_slot_if_needed(&self) {
        if self.aspace_slot_released.swap(true, Ordering::AcqRel) {
            return;
        }
        let aspace = self.aspace.lock().clone();
        crate::mm::release_process_slot(&aspace);
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

    /// Get the process nice value.
    pub fn nice(&self) -> i32 {
        self.nice.load(Ordering::SeqCst)
    }

    /// Set the process nice value.
    pub fn set_nice(&self, nice: i32) {
        self.nice.store(nice, Ordering::SeqCst);
    }

    /// Get the membarrier(2) registration state bitmask.
    pub fn membarrier_state(&self) -> u32 {
        self.membarrier_state.load(Ordering::SeqCst)
    }

    /// Add bits to the membarrier(2) registration state.
    pub fn register_membarrier_state(&self, state: u32) {
        self.membarrier_state.fetch_or(state, Ordering::SeqCst);
    }

    /// Get the dumpable flag (PR_GET_DUMPABLE).
    pub fn dumpable(&self) -> i32 {
        self.dumpable.load(Ordering::SeqCst)
    }

    /// Set the dumpable flag (PR_SET_DUMPABLE).
    /// Valid userspace values are 0 (SUID_DUMP_DISABLE) and 1
    /// (SUID_DUMP_USER). Callers must validate before storing.
    pub fn set_dumpable(&self, dumpable: i32) {
        self.dumpable.store(dumpable, Ordering::SeqCst);
    }

    /// Get the transparent huge page disable state (PR_GET_THP_DISABLE).
    pub fn thp_disable(&self) -> u32 {
        self.thp_disable.load(Ordering::SeqCst)
    }

    /// Set the transparent huge page disable state (PR_SET_THP_DISABLE).
    pub fn set_thp_disable(&self, thp_disable: u32) {
        self.thp_disable.store(thp_disable, Ordering::SeqCst);
    }

    /// Returns true if the process is currently job-control stopped.
    pub fn is_job_stopped(&self) -> bool {
        self.job_control.lock().stopped.is_some()
    }

    /// Mark the process stopped by `signo` and queue a `Stopped` report for the
    /// parent's `waitpid(WUNTRACED)`. Returns `true` if the caller should park.
    ///
    /// Returns `false` (and records nothing) when a `SIGCONT` arrived after the
    /// stop signal was dequeued but before this call — see
    /// [`Self::set_job_continued`] / `continue_generation`. Closing this race at
    /// the stop site lets us avoid scrubbing the pending-signal queue (which
    /// would require modifying `starry-signal`).
    pub fn set_job_stopped(&self, signo: Signo, continue_gen_snapshot: u64) -> bool {
        let mut jc = self.job_control.lock();
        if jc.continue_generation != continue_gen_snapshot {
            // A continue raced in after we observed `continue_gen_snapshot`;
            // honor it and do not stop.
            return false;
        }
        jc.stopped = Some(signo);
        jc.status = Some(JobStatus::Stopped(signo));
        true
    }

    /// Snapshot the continue generation. Taken right after a stop signal is
    /// dequeued and passed to [`Self::set_job_stopped`]; any intervening
    /// `SIGCONT` advances the generation and cancels the stop.
    pub fn continue_generation(&self) -> u64 {
        self.job_control.lock().continue_generation
    }

    /// Continue a stopped process: clear the stop, queue a `Continued` report,
    /// and wake parked threads. Returns true if it had been stopped.
    ///
    /// Always advances `continue_generation` so a concurrent stop in progress
    /// (signal already dequeued, not yet parked) observes the continue and
    /// skips parking.
    pub fn set_job_continued(&self) -> bool {
        let mut jc = self.job_control.lock();
        jc.continue_generation = jc.continue_generation.wrapping_add(1);
        let was_stopped = jc.stopped.take().is_some();
        if was_stopped {
            jc.status = Some(JobStatus::Continued);
            drop(jc);
            // Wake only when a thread was actually parked; avoids spurious
            // wakeups on SIGCONT to an already-running process.
            self.cont_event.wake();
        }
        was_stopped
    }

    /// Force-clear the stop (for `SIGKILL`) so a parked thread re-checks and
    /// proceeds to terminate. Does not queue a `Continued` report.
    pub fn clear_job_stop_for_kill(&self) {
        let was_stopped = self.job_control.lock().stopped.take().is_some();
        if was_stopped {
            self.cont_event.wake();
        }
    }

    /// The wait queue woken when the process is continued or killed.
    pub fn cont_event(&self) -> Arc<PollSet> {
        self.cont_event.clone()
    }

    /// Peek the pending job-control status report (without consuming it) if it
    /// matches a kind the caller's `waitpid` flags allow (`WUNTRACED` for
    /// stopped, `WCONTINUED` for continued).
    pub fn peek_job_status_if(
        &self,
        want_stopped: bool,
        want_continued: bool,
    ) -> Option<JobStatus> {
        let jc = self.job_control.lock();
        match jc.status {
            Some(s @ JobStatus::Stopped(_)) if want_stopped => Some(s),
            Some(s @ JobStatus::Continued) if want_continued => Some(s),
            _ => None,
        }
    }

    /// Consume the pending job-control status report if it matches a kind the
    /// caller's `waitpid` flags allow. Mirrors [`Self::peek_job_status_if`] but
    /// clears the slot; call it only after the status has been published to
    /// userspace so a faulting copy leaves the report intact to retry.
    pub fn take_job_status_if(
        &self,
        want_stopped: bool,
        want_continued: bool,
    ) -> Option<JobStatus> {
        let mut jc = self.job_control.lock();
        match jc.status {
            Some(JobStatus::Stopped(_)) if want_stopped => jc.status.take(),
            Some(JobStatus::Continued) if want_continued => jc.status.take(),
            _ => None,
        }
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

    /// Mark this process as traceable by its parent.
    pub fn set_ptrace_traceme(&self) {
        if let Some(parent) = self.proc.parent() {
            self.set_ptrace_tracer_pid(parent.pid());
        }
        self.ptrace_traceme.store(true, Ordering::Release);
    }

    pub fn clear_ptrace_traceme(&self) {
        self.ptrace_traceme.store(false, Ordering::Release);
    }

    pub fn is_ptrace_traceme(&self) -> bool {
        self.ptrace_traceme.load(Ordering::Acquire)
    }

    pub fn set_ptrace_tracer_pid(&self, pid: starry_process::Pid) {
        self.ptrace_tracer_pid.store(pid, Ordering::Release);
    }

    pub fn clear_ptrace_tracer_pid(&self) {
        self.ptrace_tracer_pid.store(0, Ordering::Release);
    }

    pub fn ptrace_tracer_pid(&self) -> Option<starry_process::Pid> {
        let pid = self.ptrace_tracer_pid.load(Ordering::Acquire);
        if pid == 0 { None } else { Some(pid) }
    }

    /// Record that this tracee is stopped by `signo`.
    pub fn set_ptrace_stop(&self, tid: u32, signo: Signo, uctx: &UserContext) {
        let pending_event = self.ptrace_pending_event.lock().remove(&tid);
        self.ptrace_stop.lock().insert(
            tid,
            PtraceStopRecord {
                signo: Some(signo),
                uctx: *uctx,
                siginfo: Some(SignalInfo::new_kernel(signo)),
                is_syscall: false,
                reported: false,
                event: pending_event.as_ref().map_or(0, |event| event.event),
                event_msg: pending_event.as_ref().map_or(0, |event| event.msg),
            },
        );
        self.ptrace_stop_tid.store(tid, Ordering::Release);
    }

    /// Record that this tracee is stopped at a syscall entry or exit boundary.
    pub fn set_ptrace_syscall_stop(&self, tid: u32, signo: Signo, uctx: &UserContext) {
        self.set_ptrace_stop(tid, signo, uctx);
        if let Some(stop) = self.ptrace_stop.lock().get_mut(&tid) {
            stop.is_syscall = true;
        }
    }

    pub fn ptrace_stop_tid(&self) -> Option<u32> {
        let stops = self.ptrace_stop.lock();
        stops
            .iter()
            .find_map(|(tid, stop)| (!stop.reported && stop.signo.is_some()).then_some(*tid))
            .or_else(|| stops.keys().next().copied())
    }

    pub fn select_ptrace_stop(&self, tid: u32) -> bool {
        if self.ptrace_stop.lock().contains_key(&tid) {
            self.ptrace_stop_tid.store(tid, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn selected_ptrace_stop_tid(&self) -> Option<u32> {
        let selected = self.ptrace_stop_tid.load(Ordering::Acquire);
        let stops = self.ptrace_stop.lock();
        if selected != 0
            && stops
                .get(&selected)
                .is_some_and(|stop| stop.signo.is_some())
        {
            Some(selected)
        } else {
            stops
                .iter()
                .find_map(|(tid, stop)| stop.signo.is_some().then_some(*tid))
        }
    }

    pub fn has_ptrace_stop(&self, tid: u32) -> bool {
        self.ptrace_stop.lock().contains_key(&tid)
    }

    pub fn ptrace_stop_signo_for(&self, tid: u32) -> Option<Signo> {
        self.ptrace_stop
            .lock()
            .get(&tid)
            .and_then(|stop| stop.signo)
    }

    pub fn ptrace_unreported_stop(&self, preferred_tid: Option<u32>) -> Option<(u32, Signo)> {
        {
            let stops = self.ptrace_stop.lock();
            if let Some(tid) = preferred_tid
                && let Some(stop) = stops.get(&tid)
                && !stop.reported
                && let Some(signo) = stop.signo
            {
                return Some((tid, signo));
            }
            if let Some((tid, stop)) = stops
                .iter()
                .find(|(_, stop)| !stop.reported && stop.signo.is_some() && stop.event != 0)
            {
                return stop.signo.map(|signo| (*tid, signo));
            }
        }

        if !self.ptrace_pending_event.lock().is_empty() {
            return None;
        }

        self.ptrace_stop.lock().iter().find_map(|(tid, stop)| {
            (!stop.reported)
                .then_some(stop.signo)
                .flatten()
                .map(|signo| (*tid, signo))
        })
    }

    pub fn ptrace_unreported_stop_for(&self, tid: u32) -> Option<(u32, Signo)> {
        self.ptrace_stop.lock().get(&tid).and_then(|stop| {
            (!stop.reported)
                .then_some(stop.signo)
                .flatten()
                .map(|signo| (tid, signo))
        })
    }

    pub fn is_ptrace_syscall_stop(&self) -> bool {
        let Some(tid) = self.selected_ptrace_stop_tid() else {
            return false;
        };
        self.is_ptrace_syscall_stop_for(tid)
    }

    pub fn is_ptrace_syscall_stop_for(&self, tid: u32) -> bool {
        self.ptrace_stop
            .lock()
            .get(&tid)
            .is_some_and(|stop| stop.is_syscall)
    }

    /// Return the siginfo for the current ptrace stop.
    pub fn ptrace_stop_siginfo(&self) -> Option<SignalInfo> {
        let tid = self.selected_ptrace_stop_tid()?;
        self.ptrace_stop_siginfo_for(tid)
    }

    pub fn ptrace_stop_siginfo_for(&self, tid: u32) -> Option<SignalInfo> {
        self.ptrace_stop
            .lock()
            .get(&tid)
            .and_then(|stop| stop.siginfo.clone())
    }

    /// Replace the siginfo held for the current ptrace stop.
    pub fn set_ptrace_stop_siginfo(&self, signo: Signo, siginfo: SignalInfo) -> bool {
        let Some(tid) = self.selected_ptrace_stop_tid() else {
            return false;
        };
        self.set_ptrace_stop_siginfo_for(tid, signo, siginfo)
    }

    pub fn set_ptrace_stop_siginfo_for(&self, tid: u32, signo: Signo, siginfo: SignalInfo) -> bool {
        let mut stops = self.ptrace_stop.lock();
        let Some(stop) = stops.get_mut(&tid) else {
            return false;
        };
        stop.signo = Some(signo);
        stop.siginfo = Some(siginfo);
        true
    }

    /// Return the current ptrace stop signal, if any.
    pub fn ptrace_stop_signo(&self) -> Option<Signo> {
        let stops = self.ptrace_stop.lock();
        stops
            .values()
            .find_map(|stop| (!stop.reported).then_some(stop.signo).flatten())
            .or_else(|| stops.values().find_map(|stop| stop.signo))
    }

    pub fn claim_ptrace_stop(&self, tid: u32) -> bool {
        !self.ptrace_stop.lock().contains_key(&tid)
    }

    /// Return the saved user context for the current ptrace stop.
    pub fn ptrace_stop_user_context(&self) -> Option<UserContext> {
        let tid = self.selected_ptrace_stop_tid()?;
        self.ptrace_stop_user_context_for(tid)
    }

    pub fn ptrace_stop_user_context_for(&self, tid: u32) -> Option<UserContext> {
        self.ptrace_stop.lock().get(&tid).map(|stop| stop.uctx)
    }

    pub fn ptrace_stop_reported(&self) -> bool {
        self.ptrace_stop
            .lock()
            .values()
            .all(|stop| stop.reported || stop.signo.is_none())
    }

    pub fn mark_ptrace_stop_reported(&self) {
        let mut stops = self.ptrace_stop.lock();
        if let Some(stop) = stops
            .values_mut()
            .find(|stop| !stop.reported && stop.signo.is_some())
        {
            stop.reported = true;
        }
    }

    pub fn mark_ptrace_stop_reported_for(&self, tid: u32) {
        if let Some(stop) = self.ptrace_stop.lock().get_mut(&tid) {
            stop.reported = true;
        }
    }

    /// Replace registers held for a stopped tracee.
    pub fn set_ptrace_stop_user_context(&self, uctx: UserContext) -> bool {
        let Some(tid) = self.selected_ptrace_stop_tid() else {
            return false;
        };
        self.set_ptrace_stop_user_context_for(tid, uctx)
    }

    pub fn set_ptrace_stop_user_context_for(&self, tid: u32, uctx: UserContext) -> bool {
        let mut stops = self.ptrace_stop.lock();
        let Some(stop) = stops.get_mut(&tid) else {
            return false;
        };
        stop.uctx = uctx;
        true
    }

    /// Resume the stopped task, optionally injecting a signal.
    pub fn resume_ptrace_stop_with_signal(&self, signo: u32) {
        if let Some(tid) = self.selected_ptrace_stop_tid() {
            self.resume_ptrace_stop_with_signal_for(tid, signo);
        }
    }

    pub fn resume_ptrace_stop_with_signal_for(&self, tid: u32, signo: u32) {
        if let Some(stop) = self.ptrace_stop.lock().get_mut(&tid) {
            self.ptrace_resume_signo.lock().insert(tid, signo);
            stop.signo = None;
            stop.siginfo = None;
            stop.is_syscall = false;
            stop.reported = false;
            stop.event = 0;
            stop.event_msg = 0;
        }
        self.ptrace_stop_event.wake();
    }

    /// Resume the stopped task without injecting a signal.
    pub fn resume_ptrace_stop(&self) {
        self.resume_ptrace_stop_with_signal(0);
    }

    /// Consume the signal chosen by the tracer on resume.
    pub fn take_ptrace_resume_signo_for(&self, tid: u32) -> Option<Signo> {
        let signo = self.ptrace_resume_signo.lock().remove(&tid).unwrap_or(0);
        Signo::from_repr(signo as u8)
    }

    pub fn set_ptrace_resume_signal_bypass_for(&self, tid: u32, signo: Signo) {
        self.ptrace_resume_signal_bypass
            .lock()
            .insert(tid, signo as u32);
    }

    pub fn take_ptrace_resume_signal_bypass_for(&self, tid: u32, signo: Signo) -> bool {
        let mut bypass = self.ptrace_resume_signal_bypass.lock();
        if bypass.get(&tid).copied() == Some(signo as u32) {
            bypass.remove(&tid);
            true
        } else {
            false
        }
    }

    /// Take registers once the stopped task resumes.
    pub fn take_ptrace_stop_user_context(&self) -> Option<UserContext> {
        let tid = self.selected_ptrace_stop_tid()?;
        self.take_ptrace_stop_user_context_for(tid)
    }

    pub fn take_ptrace_stop_user_context_for(&self, tid: u32) -> Option<UserContext> {
        let uctx = self.ptrace_stop.lock().remove(&tid).map(|stop| stop.uctx);
        if uctx.is_some() && self.ptrace_stop_tid.load(Ordering::Acquire) == tid {
            self.ptrace_stop_tid.store(0, Ordering::Release);
        }
        uctx
    }

    /// Cancel the current ptrace stop and discard its saved registers.
    pub fn clear_ptrace_stop(&self) {
        self.ptrace_stop.lock().clear();
        self.ptrace_stop_tid.store(0, Ordering::Release);
        self.ptrace_resume_signo.lock().clear();
        self.ptrace_resume_signal_bypass.lock().clear();
        self.ptrace_pending_event.lock().clear();
        self.ptrace_singlestep_tid.store(0, Ordering::Release);
        self.ptrace_syscall_trace.lock().clear();
        self.ptrace_ss_saved_insn.lock().clear();
        self.ptrace_stop_fp_data.lock().clear();
        self.ptrace_stop_event.wake();
    }

    pub fn set_ptrace_exec_stop_pending(&self) {
        self.ptrace_exec_stop_pending
            .store(true, core::sync::atomic::Ordering::Release);
    }

    pub fn take_ptrace_exec_stop_pending(&self) -> bool {
        self.ptrace_exec_stop_pending
            .swap(false, core::sync::atomic::Ordering::AcqRel)
    }

    /// Register a waiter for changes to this process's ptrace stop state.
    pub fn register_ptrace_stop_waker(&self, waker: &core::task::Waker) {
        self.ptrace_stop_event.register(waker);
    }

    pub fn set_ptrace_attached(&self) {
        self.ptrace_attached.store(true, Ordering::Release);
    }

    pub fn clear_ptrace_attached(&self) {
        self.ptrace_attached.store(false, Ordering::Release);
    }

    pub fn is_ptrace_attached(&self) -> bool {
        self.ptrace_attached.load(Ordering::Acquire)
    }

    pub fn set_ptrace_singlestep(&self, val: bool) {
        if !val {
            self.ptrace_singlestep_tid.store(0, Ordering::Release);
        } else if let Some(tid) = self.selected_ptrace_stop_tid() {
            self.ptrace_singlestep_tid.store(tid, Ordering::Release);
        }
    }

    pub fn set_ptrace_singlestep_for(&self, tid: u32, val: bool) {
        self.ptrace_singlestep_tid
            .store(if val { tid } else { 0 }, Ordering::Release);
    }

    pub fn is_ptrace_singlestep(&self) -> bool {
        self.ptrace_singlestep_tid.load(Ordering::Acquire) != 0
    }

    pub fn is_ptrace_singlestep_for(&self, tid: u32) -> bool {
        self.ptrace_singlestep_tid.load(Ordering::Acquire) == tid
    }

    pub fn set_ptrace_syscall_trace(&self, trace: bool) {
        if let Some(tid) = self.selected_ptrace_stop_tid() {
            self.set_ptrace_syscall_trace_for(tid, trace);
        }
    }

    pub fn set_ptrace_syscall_trace_for(&self, tid: u32, trace: bool) {
        self.set_ptrace_syscall_trace_state_for(
            tid,
            if trace {
                SyscallTraceState::Entry
            } else {
                SyscallTraceState::None
            },
        );
    }

    pub fn set_ptrace_syscall_trace_state_for(&self, tid: u32, state: SyscallTraceState) {
        let mut traces = self.ptrace_syscall_trace.lock();
        if matches!(state, SyscallTraceState::None) {
            traces.remove(&tid);
        } else {
            traces.insert(tid, state);
        }
    }

    pub fn take_ptrace_syscall_trace_for(&self, tid: u32) -> SyscallTraceState {
        self.ptrace_syscall_trace
            .lock()
            .remove(&tid)
            .unwrap_or_default()
    }

    pub fn set_ptrace_options(&self, opts: usize) {
        self.ptrace_options.store(opts, Ordering::Release);
    }

    pub fn ptrace_options(&self) -> usize {
        self.ptrace_options.load(Ordering::Acquire)
    }

    pub fn ptrace_event_msg(&self) -> usize {
        if let Some(tid) = self.selected_ptrace_stop_tid() {
            return self.ptrace_event_msg_for(tid);
        }
        0
    }

    pub fn ptrace_event_msg_for(&self, tid: u32) -> usize {
        self.ptrace_stop
            .lock()
            .get(&tid)
            .map_or(0, |stop| stop.event_msg)
    }

    pub fn set_ptrace_pending_event(&self, tid: u32, event: u32, msg: usize) {
        self.ptrace_pending_event
            .lock()
            .insert(tid, PtracePendingEvent { event, msg });
    }

    pub fn has_ptrace_pending_event_for(&self, tid: u32) -> bool {
        self.ptrace_pending_event.lock().contains_key(&tid)
    }

    pub fn ptrace_event(&self) -> Option<u32> {
        if let Some(tid) = self.selected_ptrace_stop_tid() {
            return self.ptrace_event_for(tid);
        }
        let stops = self.ptrace_stop.lock();
        let event = stops
            .values()
            .find_map(|stop| (!stop.reported && stop.event != 0).then_some(stop.event))
            .or_else(|| {
                stops
                    .values()
                    .find_map(|stop| (stop.event != 0).then_some(stop.event))
            })
            .unwrap_or(0);
        if event == 0 { None } else { Some(event) }
    }

    pub fn ptrace_event_for(&self, tid: u32) -> Option<u32> {
        let event = self
            .ptrace_stop
            .lock()
            .get(&tid)
            .map_or(0, |stop| stop.event);
        (event != 0).then_some(event)
    }

    pub fn take_ptrace_event(&self) -> Option<u32> {
        let tid = self.selected_ptrace_stop_tid()?;
        self.take_ptrace_event_for(tid)
    }

    pub fn take_ptrace_event_for(&self, tid: u32) -> Option<u32> {
        let event = self.ptrace_stop.lock().get_mut(&tid).map_or(0, |stop| {
            let event = stop.event;
            stop.event = 0;
            stop.event_msg = 0;
            event
        });
        if event == 0 { None } else { Some(event) }
    }

    pub fn set_ptrace_ss_saved_insn_for(&self, tid: u32, saved: Option<(usize, usize)>) {
        let mut saved_insns = self.ptrace_ss_saved_insn.lock();
        if let Some(saved) = saved {
            saved_insns.insert(tid, saved);
        } else {
            saved_insns.remove(&tid);
        }
    }

    pub fn take_ptrace_ss_saved_insn_for(&self, tid: u32) -> Option<(usize, usize)> {
        self.ptrace_ss_saved_insn.lock().remove(&tid)
    }

    #[cfg(target_arch = "riscv64")]
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
        let mut fp = ax_cpu::FpState::default();
        fp.save();
        fp.fs = riscv::register::sstatus::read().fs();
        self.ptrace_stop_fp_data
            .lock()
            .insert(tid, (fp.fp, fp.fcsr));
    }

    #[cfg(not(target_arch = "riscv64"))]
    pub fn save_current_fp_for_ptrace(&self, _tid: u32) {}

    #[cfg(target_arch = "riscv64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, uctx: &mut UserContext) {
        let Some((fp, fcsr)) = self.ptrace_stop_fp_data.lock().remove(&tid) else {
            return;
        };

        let fp_state = ax_cpu::FpState {
            fp,
            fcsr,
            fs: riscv::register::sstatus::FS::Dirty,
        };

        unsafe {
            riscv::register::sstatus::set_fs(riscv::register::sstatus::FS::Dirty);
        }
        fp_state.restore();
        uctx.sstatus.set_fs(riscv::register::sstatus::FS::Dirty);
    }

    #[cfg(not(target_arch = "riscv64"))]
    pub fn restore_current_fp_for_ptrace(&self, _tid: u32, _uctx: &mut UserContext) {}

    pub fn ptrace_stop_fp_data_for(&self, tid: u32) -> Option<([u64; 32], usize)> {
        self.ptrace_stop_fp_data.lock().get(&tid).copied()
    }

    pub fn set_ptrace_stop_fp_data_for(&self, tid: u32, data: ([u64; 32], usize)) -> bool {
        self.ptrace_stop_fp_data.lock().insert(tid, data).is_some()
    }

    pub fn personality(&self) -> usize {
        self.personality.load(Ordering::Acquire)
    }

    pub fn replace_personality(&self, personality: usize) -> usize {
        self.personality.swap(personality, Ordering::AcqRel)
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
        let old = {
            let mut guard = self.aspace.lock();
            core::mem::replace(&mut *guard, new_aspace)
        };
        crate::mm::release_process_slot(&old);
        let aspace_arc = self.aspace.lock().clone();
        crate::mm::attach_process_slot(&aspace_arc);
    }

    /// Set the vfork completion (called on the child after a vfork,
    /// before the child task is spawned).
    pub fn set_vfork_done(&self, poll: Arc<PollSet>) {
        *self.vfork_done.lock() = Some(VforkDone::new(poll));
    }

    /// Wait for vfork completion. Returns immediately if already done.
    /// This should be called by the parent after spawning the vfork child.
    ///
    /// The wait is killable but not arbitrarily signal-interruptible
    /// (mirroring Linux's `wait_for_completion_killable`):
    ///
    ///   - If the child notifies (exec or exit), we return normally.
    ///   - If another thread in this parent process does `execve` it will
    ///     zap us by setting `exit_request`. We bail and let the user-
    ///     return path consume `exit_request` and route to
    ///     `do_exit(0, false)`. Without this, `WaitQueue::wait_until`
    ///     would never observe the zap and the execve initiator would
    ///     deadlock in its sibling-teardown loop.
    ///   - Non-fatal signal wakeups must not unblock us: returning early
    ///     while the child still shares our address space would violate
    ///     the vfork contract. We re-enter the wait in that case.
    pub fn wait_vfork_done(&self) {
        let poll = {
            let guard = self.vfork_done.lock();
            match guard.as_ref() {
                Some(vfork) => vfork.poll.clone(),
                None => return, // No vfork, shouldn't happen but be safe.
            }
        };
        let curr_task = ax_task::current();
        let curr_thr = curr_task.as_thread();
        loop {
            let result = ax_task::future::block_on(ax_task::future::interruptible(
                core::future::poll_fn(|cx| {
                    // Register before re-checking so a notify that fires
                    // between our last check and this register isn't lost.
                    poll.register(cx.waker());
                    let done = self
                        .vfork_done
                        .lock()
                        .as_ref()
                        .map(|v| v.done)
                        .unwrap_or(true);
                    if done {
                        core::task::Poll::Ready(())
                    } else {
                        core::task::Poll::Pending
                    }
                }),
            ));
            match result {
                Ok(()) => return,
                Err(_) => {
                    if curr_thr.has_exit_request() {
                        return;
                    }
                    // Spurious wake from a non-fatal signal; keep waiting.
                    continue;
                }
            }
        }
    }

    /// Notify the vfork parent that this child has exec'd or exited.
    /// No-op if this process was not created by vfork.
    pub fn notify_vfork_done(&self) {
        // Set done under the lock, then drop the lock before notifying
        // to avoid lock-order inversion with the poll-set internal lock.
        let poll = {
            let mut guard = self.vfork_done.lock();
            match guard.as_mut() {
                Some(vfork) => {
                    vfork.done = true;
                    vfork.poll.clone()
                }
                None => return,
            }
            // guard dropped here
        };
        poll.wake();
    }
}

impl Drop for ProcessData {
    fn drop(&mut self) {
        self.release_aspace_slot_if_needed();
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
