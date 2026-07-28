//! User task management.

mod cred;
pub mod futex;
pub mod future;
mod job_control;
mod ops;
pub mod posix_timer;
mod process_accounting;
mod process_identity;
mod process_image;
mod process_memory;
mod process_policy;
mod process_wait;
mod resources;
mod scheduler_identity;
mod scheduler_task;
mod seccomp;
mod signal;
mod signal_publication;
mod stat;
mod thread;
mod tid;
mod timer;
mod user;
mod user_wait;

use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use ax_kspin::SpinRwLock as RwLock;
use ax_runtime::hal::cpu::uspace::UserContext;
use ax_sync::{PiMutex, spin::SpinNoIrq};
use axpoll::{IoEvents, PollSet};
use starry_process::{Pid, Process};
use starry_signal::{
    SignalInfo, Signo,
    api::{ProcessSignalManager, SignalActions},
};

use crate::mm::AddrSpace;

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

pub(crate) use self::process_identity::*;
pub use self::{
    cred::*, futex::*, job_control::JobStatus, ops::*, posix_timer::PosixTimerTable,
    process_image::ProcessImage, process_wait::wait_on_pollset, resources::*, scheduler_task::*,
    seccomp::*, signal::*, stat::*, thread::Thread, tid::*, timer::*, user::*,
};
use self::{
    job_control::ProcessJobControl, process_accounting::ProcessAccountingState,
    process_image::ProcessImageState, process_memory::ProcessMemoryState,
    process_policy::ProcessPolicyState, process_wait::ProcessWaitState,
};
#[cfg(axtest)]
pub(crate) use self::{
    ops::decode_wait_status_rules_hold_for_test,
    posix_timer::posix_timer_clock_validation_rules_hold_for_test,
    seccomp::seccomp_action_and_precedence_rules_hold_for_test,
    seccomp::seccomp_bpf_constants_hold_for_test,
    timer::itimer_type_signo_and_time_conversion_rules_hold_for_test,
};

pub struct ProcessData {
    /// The process.
    pub proc: Arc<Process>,
    /// Stable generation identity shared by the registry and pidfds.
    identity: Arc<ProcessIdentity>,
    /// Executable metadata independently synchronized for exec and procfs.
    image: ProcessImageState,
    /// Address-space publication and release state.
    memory: ProcessMemoryState,
    /// The per-process uprobe manager. Each process has its own because user
    /// code can be modified independently.
    pub uprobe_manager: crate::kprobe::KprobeManager,
    /// Per-process uprobe point list, paired with [`Self::uprobe_manager`].
    pub uprobe_point_list: PiMutex<crate::kprobe::KprobePointList>,
    /// The namespace proxy — aggregates all namespace types for this process.
    pub nsproxy: SpinNoIrq<axnsproxy::NsProxy>,
    /// Authoritative cgroup membership shared by every thread in the process.
    pub cgroup: RwLock<Arc<ax_cgroup::CgroupNode>>,
    /// Resource limits and process-wide compatibility policy.
    policy: ProcessPolicyState,

    /// Exit metadata, wait channels, and vfork completion.
    wait: ProcessWaitState,

    /// The process signal manager
    pub signal: Arc<ProcessSignalManager>,

    /// The futex table.
    futex_table: Arc<FutexTable>,

    /// CPU accounting and process-owned timer tables.
    accounting: ProcessAccountingState,

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
    ptrace_stop_fp_data: SpinNoIrq<BTreeMap<u32, PtraceStopFpData>>,

    /// Job-control stop state and parent-report delivery.
    job_control: ProcessJobControl,
}

#[cfg(target_arch = "riscv64")]
#[derive(Clone, Copy)]
pub struct PtraceStopFpData {
    pub regs: [u64; 32],
    pub fcsr: usize,
}

#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
pub struct PtraceStopFpData {
    pub regs: [u128; 32],
    pub fpcr: u32,
    pub fpsr: u32,
}

#[cfg(target_arch = "loongarch64")]
#[derive(Clone, Copy)]
pub struct PtraceStopFpData {
    pub regs: [u64; 32],
    pub fp_high: [u64; 32],
    pub fp_lasx_hi0: [u64; 32],
    pub fp_lasx_hi1: [u64; 32],
    pub fcc: [u8; 8],
    pub fcsr: u32,
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
#[derive(Clone, Copy)]
pub struct PtraceStopFpData;

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy)]
pub struct PtraceStopFpData(pub ax_cpu::FxsaveArea);

impl ProcessData {
    /// Create a new [`ProcessData`].
    pub fn new(
        proc: Arc<Process>,
        image: ProcessImage,
        aspace: Arc<PiMutex<AddrSpace>>,
        signal_actions: Arc<SpinNoIrq<SignalActions>>,
        exit_signal: Option<Signo>,
        wait_parent_tid: Pid,
        vm_aspace_shared: bool,
    ) -> Arc<Self> {
        let this = Arc::new_cyclic(|weak| {
            let wait = ProcessWaitState::new(exit_signal, wait_parent_tid);
            let identity = ProcessIdentity::new(proc.clone(), wait.exit_event_arc(), weak.clone());
            Self {
                proc,
                identity,
                image: ProcessImageState::new(image),
                memory: ProcessMemoryState::new(aspace, vm_aspace_shared),
                wait,
                uprobe_manager: crate::kprobe::KprobeManager::new(),
                uprobe_point_list: PiMutex::new(crate::kprobe::KprobePointList::new()),

                policy: ProcessPolicyState::new(),
                accounting: ProcessAccountingState::new(),

                signal: Arc::new(ProcessSignalManager::new(
                    signal_actions,
                    crate::config::SIGNAL_TRAMPOLINE,
                )),

                futex_table: Arc::new(FutexTable::new()),

                nsproxy: SpinNoIrq::new(axnsproxy::NsProxy::new_root()),
                cgroup: RwLock::new(crate::cgroup::root()),

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

                job_control: ProcessJobControl::new(),
            }
        });
        // Clone the Arc in a separate statement: a temporary `SpinNoIrq` guard
        // from `lock()` lives until the end of the statement, so calling
        // `attach_process_slot` (which locks `PiMutex<AddrSpace>`) in the same
        // expression would nest a sleepable lock inside atomic context.
        let aspace_arc = this.aspace();
        crate::mm::attach_process_slot(&aspace_arc);
        this
    }

    /// Returns this process generation's stable PID identity.
    pub(crate) fn identity(&self) -> Arc<ProcessIdentity> {
        self.identity.clone()
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

    pub fn ptrace_stop_siginfo_for(&self, tid: u32) -> Option<SignalInfo> {
        self.ptrace_stop
            .lock()
            .get(&tid)
            .and_then(|stop| stop.siginfo)
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

    pub fn ptrace_stop_user_context_for(&self, tid: u32) -> Option<UserContext> {
        self.ptrace_stop.lock().get(&tid).map(|stop| stop.uctx)
    }

    pub fn mark_ptrace_stop_reported_for(&self, tid: u32) {
        if let Some(stop) = self.ptrace_stop.lock().get_mut(&tid) {
            stop.reported = true;
        }
    }

    pub fn set_ptrace_stop_user_context_for(&self, tid: u32, uctx: UserContext) -> bool {
        let mut stops = self.ptrace_stop.lock();
        let Some(stop) = stops.get_mut(&tid) else {
            return false;
        };
        stop.uctx = uctx;
        true
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
        // Ptrace stop state is updated before waking waiters.
        unsafe { self.ptrace_stop_event.wake(IoEvents::IN) };
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
        // Ptrace stop state is cleared before waking waiters.
        unsafe { self.ptrace_stop_event.wake(IoEvents::IN) };
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
        // Registration happens from task/wait context.
        unsafe { self.ptrace_stop_event.register(waker, IoEvents::IN) };
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

    pub fn set_ptrace_singlestep_for(&self, tid: u32, val: bool) {
        self.ptrace_singlestep_tid
            .store(if val { tid } else { 0 }, Ordering::Release);
    }

    pub fn is_ptrace_singlestep_for(&self, tid: u32) -> bool {
        self.ptrace_singlestep_tid.load(Ordering::Acquire) == tid
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

    #[cfg(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "loongarch64"
    ))]
    pub fn set_ptrace_ss_saved_insn_for(&self, tid: u32, saved: Option<(usize, usize)>) {
        let mut saved_insns = self.ptrace_ss_saved_insn.lock();
        if let Some(saved) = saved {
            saved_insns.insert(tid, saved);
        } else {
            saved_insns.remove(&tid);
        }
    }

    #[cfg(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "loongarch64"
    ))]
    pub fn take_ptrace_ss_saved_insn_for(&self, tid: u32) -> Option<(usize, usize)> {
        self.ptrace_ss_saved_insn.lock().remove(&tid)
    }

    #[cfg(target_arch = "riscv64")]
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
        let mut fp = ax_cpu::FpState::default();
        fp.save();
        fp.fs = riscv::register::sstatus::read().fs();
        self.ptrace_stop_fp_data.lock().insert(
            tid,
            PtraceStopFpData {
                regs: fp.fp,
                fcsr: fp.fcsr,
            },
        );
    }

    #[cfg(target_arch = "aarch64")]
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
        let mut fp = ax_cpu::FpState::default();
        fp.save();
        self.ptrace_stop_fp_data.lock().insert(
            tid,
            PtraceStopFpData {
                regs: fp.regs,
                fpcr: fp.fpcr,
                fpsr: fp.fpsr,
            },
        );
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
        let mut fp = ax_cpu::FpuState::default();
        fp.save();
        self.ptrace_stop_fp_data.lock().insert(
            tid,
            PtraceStopFpData {
                regs: fp.fp,
                fp_high: fp.fp_high,
                fp_lasx_hi0: fp.fp_lasx_hi0,
                fp_lasx_hi1: fp.fp_lasx_hi1,
                fcc: fp.fcc,
                fcsr: fp.fcsr,
            },
        );
    }

    #[cfg(not(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    )))]
    pub fn save_current_fp_for_ptrace(&self, _tid: u32) {}

    #[cfg(target_arch = "x86_64")]
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
        let mut area =
            unsafe { core::mem::MaybeUninit::<ax_cpu::FxsaveArea>::zeroed().assume_init() };
        unsafe {
            core::arch::x86_64::_fxsave64((&mut area as *mut ax_cpu::FxsaveArea).cast::<u8>());
        }
        self.ptrace_stop_fp_data
            .lock()
            .insert(tid, PtraceStopFpData(area));
    }

    #[cfg(target_arch = "riscv64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, uctx: &mut UserContext) {
        let Some(fp) = self.ptrace_stop_fp_data.lock().remove(&tid) else {
            return;
        };

        let fp_state = ax_cpu::FpState {
            fp: fp.regs,
            fcsr: fp.fcsr,
            fs: riscv::register::sstatus::FS::Dirty,
        };

        unsafe {
            riscv::register::sstatus::set_fs(riscv::register::sstatus::FS::Dirty);
        }
        fp_state.restore();
        uctx.sstatus.set_fs(riscv::register::sstatus::FS::Dirty);
    }

    #[cfg(target_arch = "aarch64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, _uctx: &mut UserContext) {
        let Some(fp) = self.ptrace_stop_fp_data.lock().remove(&tid) else {
            return;
        };

        let fp_state = ax_cpu::FpState {
            regs: fp.regs,
            fpcr: fp.fpcr,
            fpsr: fp.fpsr,
        };

        fp_state.restore();
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, _uctx: &mut UserContext) {
        let Some(fp) = self.ptrace_stop_fp_data.lock().remove(&tid) else {
            return;
        };

        let fp_state = ax_cpu::FpuState {
            fp: fp.regs,
            fp_high: fp.fp_high,
            fp_lasx_hi0: fp.fp_lasx_hi0,
            fp_lasx_hi1: fp.fp_lasx_hi1,
            fcc: fp.fcc,
            fcsr: fp.fcsr,
            _reserved: 0,
        };

        fp_state.restore();
    }

    #[cfg(not(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    )))]
    pub fn restore_current_fp_for_ptrace(&self, _tid: u32, _uctx: &mut UserContext) {}

    #[cfg(target_arch = "x86_64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, _uctx: &mut UserContext) {
        let Some(PtraceStopFpData(area)) = self.ptrace_stop_fp_data.lock().remove(&tid) else {
            return;
        };
        unsafe {
            core::arch::x86_64::_fxrstor64((&area as *const ax_cpu::FxsaveArea).cast::<u8>());
        }
    }

    pub fn ptrace_stop_fp_data_for(&self, tid: u32) -> Option<PtraceStopFpData> {
        self.ptrace_stop_fp_data.lock().get(&tid).copied()
    }

    pub fn set_ptrace_stop_fp_data_for(&self, tid: u32, data: PtraceStopFpData) -> bool {
        self.ptrace_stop_fp_data.lock().insert(tid, data).is_some()
    }
}

impl Drop for ProcessData {
    fn drop(&mut self) {
        self.release_aspace_slot_if_needed();
    }
}
