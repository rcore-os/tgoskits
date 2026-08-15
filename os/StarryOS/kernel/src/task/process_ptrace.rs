//! Ptrace ownership, stop records, and architecture register snapshots.

use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};

use ax_runtime::hal::cpu::uspace::UserContext;
use axpoll::{IoEvents, PollSet};
use starry_signal::{SignalInfo, Signo};

use super::{PidIdentity, PidSnapshot, PidView, ProcessData, TidNumber};
use crate::sync::PiMutex;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SyscallTraceState {
    #[default]
    None,
    Entry,
    Exit,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PtraceAttachMode {
    #[default]
    None,
    Attach,
    Seize,
}

struct PtraceStopRecord {
    signo: Option<Signo>,
    uctx: UserContext,
    siginfo: Option<SignalInfo>,
    kind: PtraceStopKind,
    reported: bool,
    event: u32,
    event_msg: PtraceEventMessage,
}

#[derive(Clone, Copy)]
enum PtraceStopKind {
    Signal,
    Syscall {
        #[cfg(target_arch = "x86_64")]
        number: usize,
    },
}

impl PtraceStopKind {
    fn syscall(number: usize) -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            Self::Syscall { number }
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            let _ = number;
            Self::Syscall {}
        }
    }

    fn is_syscall(self) -> bool {
        matches!(self, Self::Syscall { .. })
    }

    #[cfg(target_arch = "x86_64")]
    fn syscall_number(self) -> Option<usize> {
        match self {
            Self::Signal => None,
            Self::Syscall { number } => Some(number),
        }
    }
}

impl PtraceStopRecord {
    fn new(
        signo: Signo,
        uctx: &UserContext,
        kind: PtraceStopKind,
        pending_event: Option<PtracePendingEvent>,
    ) -> Self {
        Self {
            signo: Some(signo),
            uctx: *uctx,
            siginfo: Some(SignalInfo::new_kernel(signo)),
            kind,
            reported: false,
            event: pending_event.as_ref().map_or(0, |event| event.event),
            event_msg: pending_event
                .as_ref()
                .map_or(PtraceEventMessage::Value(0), |event| event.msg.clone()),
        }
    }
}

#[derive(Clone)]
enum PtraceEventMessage {
    Value(usize),
    Pid(PidSnapshot),
}

struct PtracePendingEvent {
    event: u32,
    msg: PtraceEventMessage,
}

/// Ptrace state owned by one process generation.
pub(super) struct ProcessPtraceState {
    tracer_identity: PiMutex<Option<Arc<PidIdentity>>>,
    traceme: AtomicBool,
    stops: PiMutex<BTreeMap<TidNumber, PtraceStopRecord>>,
    selected_tid: AtomicU32,
    stop_event: Arc<PollSet>,
    resume_signo: PiMutex<BTreeMap<TidNumber, u32>>,
    resume_signal_bypass: PiMutex<BTreeMap<TidNumber, u32>>,
    exec_stop_pending: PiMutex<Option<PidSnapshot>>,
    attach_mode: AtomicU8,
    singlestep_tid: AtomicU32,
    syscall_trace: PiMutex<BTreeMap<TidNumber, SyscallTraceState>>,
    options: AtomicUsize,
    pending_event: PiMutex<BTreeMap<TidNumber, PtracePendingEvent>>,
    ss_saved_insn: PiMutex<BTreeMap<TidNumber, (usize, usize)>>,
    stop_fp_data: PiMutex<BTreeMap<TidNumber, PtraceStopFpData>>,
}

impl ProcessPtraceState {
    pub(super) fn new() -> Self {
        Self {
            tracer_identity: PiMutex::new(None),
            traceme: AtomicBool::new(false),
            stops: PiMutex::new(BTreeMap::new()),
            selected_tid: AtomicU32::new(0),
            stop_event: Arc::default(),
            resume_signo: PiMutex::new(BTreeMap::new()),
            resume_signal_bypass: PiMutex::new(BTreeMap::new()),
            exec_stop_pending: PiMutex::new(None),
            attach_mode: AtomicU8::new(PtraceAttachMode::None as u8),
            singlestep_tid: AtomicU32::new(0),
            syscall_trace: PiMutex::new(BTreeMap::new()),
            options: AtomicUsize::new(0),
            pending_event: PiMutex::new(BTreeMap::new()),
            ss_saved_insn: PiMutex::new(BTreeMap::new()),
            stop_fp_data: PiMutex::new(BTreeMap::new()),
        }
    }

    fn publish_stop(&self, tid: TidNumber, signo: Signo, uctx: &UserContext, kind: PtraceStopKind) {
        let pending_event = self.pending_event.lock().remove(&tid);
        let stop = PtraceStopRecord::new(signo, uctx, kind, pending_event);
        self.stops.lock().insert(tid, stop);
        self.selected_tid.store(tid.get(), Ordering::Release);
    }

    /// Snapshots ptrace work for one syscall boundary.
    ///
    /// As in Linux's `syscall_work`, an untraced task takes a lock-free fast
    /// path. Traced tasks retain their entry/exit state across the stop; the
    /// tracer advances it only when resuming that exact syscall stop.
    pub(super) fn syscall_trace_if_active(&self, tid: TidNumber) -> Option<SyscallTraceState> {
        if !self.traceme.load(Ordering::Acquire)
            && self.attach_mode.load(Ordering::Acquire) == PtraceAttachMode::None as u8
        {
            return None;
        }
        Some(
            self.syscall_trace
                .lock()
                .get(&tid)
                .copied()
                .unwrap_or_default(),
        )
    }
}

#[cfg(axtest)]
pub(crate) fn inactive_ptrace_syscall_gate_is_lock_free_for_test() -> bool {
    ProcessPtraceState::new()
        .syscall_trace_if_active(TidNumber::try_from(1).unwrap())
        .is_none()
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
    /// Mark this process as traceable by its parent.
    pub fn set_ptrace_traceme(&self) {
        if let Some(parent) = self.proc.parent() {
            self.set_ptrace_tracer(&parent.identity());
        }
        self.ptrace.traceme.store(true, Ordering::Release);
    }

    pub fn clear_ptrace_traceme(&self) {
        self.ptrace.traceme.store(false, Ordering::Release);
    }

    pub fn is_ptrace_traceme(&self) -> bool {
        self.ptrace.traceme.load(Ordering::Acquire)
    }

    pub fn set_ptrace_tracer(&self, tracer: &Arc<PidIdentity>) {
        *self.ptrace.tracer_identity.lock() = Some(tracer.clone());
    }

    pub fn clear_ptrace_tracer(&self) {
        *self.ptrace.tracer_identity.lock() = None;
    }

    pub fn ptrace_tracer_identity(&self) -> Option<Arc<PidIdentity>> {
        self.ptrace.tracer_identity.lock().clone()
    }

    /// Record that this tracee is stopped by `signo`.
    pub fn set_ptrace_stop(&self, tid: TidNumber, signo: Signo, uctx: &UserContext) {
        self.ptrace
            .publish_stop(tid, signo, uctx, PtraceStopKind::Signal);
    }

    /// Record that this tracee is stopped at a syscall entry or exit boundary.
    pub fn set_ptrace_syscall_stop(
        &self,
        tid: TidNumber,
        signo: Signo,
        uctx: &UserContext,
        syscall_no: usize,
    ) {
        self.ptrace
            .publish_stop(tid, signo, uctx, PtraceStopKind::syscall(syscall_no));
    }

    pub fn ptrace_stop_tid(&self) -> Option<TidNumber> {
        let stops = self.ptrace.stops.lock();
        stops
            .iter()
            .find_map(|(tid, stop)| (!stop.reported && stop.signo.is_some()).then_some(*tid))
            .or_else(|| stops.keys().next().copied())
    }

    pub fn select_ptrace_stop(&self, tid: TidNumber) -> bool {
        if self.ptrace.stops.lock().contains_key(&tid) {
            self.ptrace.selected_tid.store(tid.get(), Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn selected_ptrace_stop_tid(&self) -> Option<TidNumber> {
        let selected = TidNumber::try_from(self.ptrace.selected_tid.load(Ordering::Acquire)).ok();
        let stops = self.ptrace.stops.lock();
        if selected.is_some_and(|tid| stops.get(&tid).is_some_and(|stop| stop.signo.is_some())) {
            selected
        } else {
            stops
                .iter()
                .find_map(|(tid, stop)| stop.signo.is_some().then_some(*tid))
        }
    }

    pub fn has_ptrace_stop(&self, tid: TidNumber) -> bool {
        self.ptrace.stops.lock().contains_key(&tid)
    }

    pub fn ptrace_stop_signo_for(&self, tid: TidNumber) -> Option<Signo> {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .and_then(|stop| stop.signo)
    }

    /// Returns whether `tid` stopped at a syscall boundary.
    pub fn ptrace_stop_is_syscall_for(&self, tid: TidNumber) -> bool {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .is_some_and(|stop| stop.kind.is_syscall())
    }

    /// Returns the original syscall number associated with a syscall stop.
    #[cfg(target_arch = "x86_64")]
    pub fn ptrace_stop_syscall_number_for(&self, tid: TidNumber) -> Option<usize> {
        self.ptrace.stops.lock().get(&tid)?.kind.syscall_number()
    }

    pub fn ptrace_unreported_stop(
        &self,
        preferred_tid: Option<TidNumber>,
    ) -> Option<(TidNumber, Signo)> {
        {
            let stops = self.ptrace.stops.lock();
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

        if !self.ptrace.pending_event.lock().is_empty() {
            return None;
        }

        self.ptrace.stops.lock().iter().find_map(|(tid, stop)| {
            (!stop.reported)
                .then_some(stop.signo)
                .flatten()
                .map(|signo| (*tid, signo))
        })
    }

    pub fn ptrace_unreported_stop_for(&self, tid: TidNumber) -> Option<(TidNumber, Signo)> {
        self.ptrace.stops.lock().get(&tid).and_then(|stop| {
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

    pub fn is_ptrace_syscall_stop_for(&self, tid: TidNumber) -> bool {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .is_some_and(|stop| stop.kind.is_syscall())
    }

    pub fn ptrace_stop_siginfo_for(&self, tid: TidNumber) -> Option<SignalInfo> {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .and_then(|stop| stop.siginfo)
    }

    pub fn set_ptrace_stop_siginfo_for(
        &self,
        tid: TidNumber,
        signo: Signo,
        siginfo: SignalInfo,
    ) -> bool {
        let mut stops = self.ptrace.stops.lock();
        let Some(stop) = stops.get_mut(&tid) else {
            return false;
        };
        stop.signo = Some(signo);
        stop.siginfo = Some(siginfo);
        true
    }

    /// Return the current ptrace stop signal, if any.
    pub fn ptrace_stop_signo(&self) -> Option<Signo> {
        let stops = self.ptrace.stops.lock();
        stops
            .values()
            .find_map(|stop| (!stop.reported).then_some(stop.signo).flatten())
            .or_else(|| stops.values().find_map(|stop| stop.signo))
    }

    pub fn claim_ptrace_stop(&self, tid: TidNumber) -> bool {
        !self.ptrace.stops.lock().contains_key(&tid)
    }

    pub fn ptrace_stop_user_context_for(&self, tid: TidNumber) -> Option<UserContext> {
        self.ptrace.stops.lock().get(&tid).map(|stop| stop.uctx)
    }

    pub fn mark_ptrace_stop_reported_for(&self, tid: TidNumber) {
        if let Some(stop) = self.ptrace.stops.lock().get_mut(&tid) {
            stop.reported = true;
        }
    }

    pub fn set_ptrace_stop_user_context_for(&self, tid: TidNumber, uctx: UserContext) -> bool {
        let mut stops = self.ptrace.stops.lock();
        let Some(stop) = stops.get_mut(&tid) else {
            return false;
        };
        stop.uctx = uctx;
        true
    }

    /// Replaces the original syscall number held for a stopped tracee.
    #[cfg(target_arch = "x86_64")]
    pub fn set_ptrace_stop_syscall_number_for(&self, tid: TidNumber, syscall_no: usize) -> bool {
        let mut stops = self.ptrace.stops.lock();
        let Some(stop) = stops.get_mut(&tid) else {
            return false;
        };
        match &mut stop.kind {
            PtraceStopKind::Signal => false,
            PtraceStopKind::Syscall { number } => {
                *number = syscall_no;
                true
            }
        }
    }

    pub fn resume_ptrace_stop_with_signal_for(&self, tid: TidNumber, signo: u32) {
        if let Some(stop) = self.ptrace.stops.lock().get_mut(&tid) {
            self.ptrace.resume_signo.lock().insert(tid, signo);
            stop.signo = None;
            stop.siginfo = None;
            stop.kind = PtraceStopKind::Signal;
            stop.reported = false;
            stop.event = 0;
            stop.event_msg = PtraceEventMessage::Value(0);
        }
        // Ptrace stop state is updated before waking waiters.
        unsafe { self.ptrace.stop_event.wake(IoEvents::IN) };
    }

    /// Consume the signal chosen by the tracer on resume.
    pub fn take_ptrace_resume_signo_for(&self, tid: TidNumber) -> Option<Signo> {
        let signo = self.ptrace.resume_signo.lock().remove(&tid).unwrap_or(0);
        Signo::from_repr(signo as u8)
    }

    pub fn set_ptrace_resume_signal_bypass_for(&self, tid: TidNumber, signo: Signo) {
        self.ptrace
            .resume_signal_bypass
            .lock()
            .insert(tid, signo as u32);
    }

    pub fn take_ptrace_resume_signal_bypass_for(&self, tid: TidNumber, signo: Signo) -> bool {
        let mut bypass = self.ptrace.resume_signal_bypass.lock();
        if bypass.get(&tid).copied() == Some(signo as u32) {
            bypass.remove(&tid);
            true
        } else {
            false
        }
    }

    pub fn take_ptrace_stop_user_context_for(&self, tid: TidNumber) -> Option<UserContext> {
        let uctx = self.ptrace.stops.lock().remove(&tid).map(|stop| stop.uctx);
        if uctx.is_some() && self.ptrace.selected_tid.load(Ordering::Acquire) == tid.get() {
            self.ptrace.selected_tid.store(0, Ordering::Release);
        }
        uctx
    }

    /// Cancel the current ptrace stop and discard its saved registers.
    pub fn clear_ptrace_stop(&self) {
        self.ptrace.stops.lock().clear();
        self.ptrace.selected_tid.store(0, Ordering::Release);
        self.ptrace.resume_signo.lock().clear();
        self.ptrace.resume_signal_bypass.lock().clear();
        self.ptrace.pending_event.lock().clear();
        self.ptrace.singlestep_tid.store(0, Ordering::Release);
        self.ptrace.syscall_trace.lock().clear();
        self.ptrace.ss_saved_insn.lock().clear();
        self.ptrace.stop_fp_data.lock().clear();
        // Ptrace stop state is cleared before waking waiters.
        unsafe { self.ptrace.stop_event.wake(IoEvents::IN) };
    }

    pub fn set_ptrace_exec_stop_pending(&self, former_tid: PidSnapshot) {
        *self.ptrace.exec_stop_pending.lock() = Some(former_tid);
    }

    pub fn take_ptrace_exec_stop_pending(&self) -> Option<PidSnapshot> {
        self.ptrace.exec_stop_pending.lock().take()
    }

    /// Register a waiter for changes to this process's ptrace stop state.
    pub fn register_ptrace_stop_waker(&self, waker: &core::task::Waker) {
        // Registration happens from task/wait context.
        unsafe { self.ptrace.stop_event.register(waker, IoEvents::IN) };
    }

    pub(crate) fn set_ptrace_attach_mode(&self, mode: PtraceAttachMode) {
        self.ptrace.attach_mode.store(mode as u8, Ordering::Release);
    }

    pub fn clear_ptrace_attached(&self) {
        self.set_ptrace_attach_mode(PtraceAttachMode::None);
    }

    pub(crate) fn ptrace_attach_mode(&self) -> PtraceAttachMode {
        match self.ptrace.attach_mode.load(Ordering::Acquire) {
            value if value == PtraceAttachMode::Attach as u8 => PtraceAttachMode::Attach,
            value if value == PtraceAttachMode::Seize as u8 => PtraceAttachMode::Seize,
            _ => PtraceAttachMode::None,
        }
    }

    pub fn is_ptrace_attached(&self) -> bool {
        self.ptrace_attach_mode() != PtraceAttachMode::None
    }

    pub fn is_ptrace_seized(&self) -> bool {
        self.ptrace_attach_mode() == PtraceAttachMode::Seize
    }

    pub fn set_ptrace_singlestep_for(&self, tid: TidNumber, val: bool) {
        self.ptrace
            .singlestep_tid
            .store(if val { tid.get() } else { 0 }, Ordering::Release);
    }

    pub fn is_ptrace_singlestep_for(&self, tid: TidNumber) -> bool {
        self.ptrace.singlestep_tid.load(Ordering::Acquire) == tid.get()
    }

    pub fn set_ptrace_syscall_trace_for(&self, tid: TidNumber, trace: bool) {
        self.set_ptrace_syscall_trace_state_for(
            tid,
            if trace {
                SyscallTraceState::Entry
            } else {
                SyscallTraceState::None
            },
        );
    }

    pub fn set_ptrace_syscall_trace_state_for(&self, tid: TidNumber, state: SyscallTraceState) {
        let mut traces = self.ptrace.syscall_trace.lock();
        if matches!(state, SyscallTraceState::None) {
            traces.remove(&tid);
        } else {
            traces.insert(tid, state);
        }
    }

    /// Returns the next syscall boundary at which `tid` must stop.
    pub fn ptrace_syscall_trace_state_for(&self, tid: TidNumber) -> SyscallTraceState {
        self.ptrace
            .syscall_trace
            .lock()
            .get(&tid)
            .copied()
            .unwrap_or_default()
    }

    /// Advances syscall tracing to the opposite boundary.
    pub fn advance_ptrace_syscall_trace_for(&self, tid: TidNumber) {
        let mut traces = self.ptrace.syscall_trace.lock();
        let next = match traces.get(&tid).copied() {
            Some(SyscallTraceState::Entry) => SyscallTraceState::Exit,
            Some(SyscallTraceState::Exit) | Some(SyscallTraceState::None) | None => {
                SyscallTraceState::Entry
            }
        };
        traces.insert(tid, next);
    }

    pub fn set_ptrace_options(&self, opts: usize) {
        self.ptrace.options.store(opts, Ordering::Release);
    }

    pub fn ptrace_options(&self) -> usize {
        self.ptrace.options.load(Ordering::Acquire)
    }

    pub fn ptrace_event_msg_for(&self, tid: TidNumber, view: &PidView) -> usize {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .map_or(0, |stop| match &stop.event_msg {
                PtraceEventMessage::Value(value) => *value,
                PtraceEventMessage::Pid(snapshot) => view
                    .visible_snapshot_number(snapshot)
                    .map_or(0, |number| number.get() as usize),
            })
    }

    pub fn set_ptrace_pending_event(&self, tid: TidNumber, event: u32, msg: usize) {
        self.ptrace.pending_event.lock().insert(
            tid,
            PtracePendingEvent {
                event,
                msg: PtraceEventMessage::Value(msg),
            },
        );
    }

    pub fn set_ptrace_pending_pid_event(&self, tid: TidNumber, event: u32, pid: PidSnapshot) {
        self.ptrace.pending_event.lock().insert(
            tid,
            PtracePendingEvent {
                event,
                msg: PtraceEventMessage::Pid(pid),
            },
        );
    }

    pub fn has_ptrace_pending_event_for(&self, tid: TidNumber) -> bool {
        self.ptrace.pending_event.lock().contains_key(&tid)
    }

    pub fn ptrace_event(&self) -> Option<u32> {
        if let Some(tid) = self.selected_ptrace_stop_tid() {
            return self.ptrace_event_for(tid);
        }
        let stops = self.ptrace.stops.lock();
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

    pub fn ptrace_event_for(&self, tid: TidNumber) -> Option<u32> {
        let event = self
            .ptrace
            .stops
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
    pub fn set_ptrace_ss_saved_insn_for(&self, tid: TidNumber, saved: Option<(usize, usize)>) {
        let mut saved_insns = self.ptrace.ss_saved_insn.lock();
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
    pub fn take_ptrace_ss_saved_insn_for(&self, tid: TidNumber) -> Option<(usize, usize)> {
        self.ptrace.ss_saved_insn.lock().remove(&tid)
    }

    #[cfg(target_arch = "riscv64")]
    pub fn save_current_fp_for_ptrace(&self, tid: TidNumber) {
        let mut fp = ax_cpu::FpState::default();
        fp.save();
        fp.fs = riscv::register::sstatus::read().fs();
        self.ptrace.stop_fp_data.lock().insert(
            tid,
            PtraceStopFpData {
                regs: fp.fp,
                fcsr: fp.fcsr,
            },
        );
    }

    #[cfg(target_arch = "aarch64")]
    pub fn save_current_fp_for_ptrace(&self, tid: TidNumber) {
        let mut fp = ax_cpu::FpState::default();
        fp.save();
        self.ptrace.stop_fp_data.lock().insert(
            tid,
            PtraceStopFpData {
                regs: fp.regs,
                fpcr: fp.fpcr,
                fpsr: fp.fpsr,
            },
        );
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn save_current_fp_for_ptrace(&self, tid: TidNumber) {
        let mut fp = ax_cpu::FpuState::default();
        fp.save();
        self.ptrace.stop_fp_data.lock().insert(
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
    pub fn save_current_fp_for_ptrace(&self, _tid: TidNumber) {}

    #[cfg(target_arch = "x86_64")]
    pub fn save_current_fp_for_ptrace(&self, tid: TidNumber) {
        let mut area =
            unsafe { core::mem::MaybeUninit::<ax_cpu::FxsaveArea>::zeroed().assume_init() };
        unsafe {
            core::arch::x86_64::_fxsave64((&mut area as *mut ax_cpu::FxsaveArea).cast::<u8>());
        }
        self.ptrace
            .stop_fp_data
            .lock()
            .insert(tid, PtraceStopFpData(area));
    }

    #[cfg(target_arch = "riscv64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: TidNumber, uctx: &mut UserContext) {
        let Some(fp) = self.ptrace.stop_fp_data.lock().remove(&tid) else {
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
    pub fn restore_current_fp_for_ptrace(&self, tid: TidNumber, _uctx: &mut UserContext) {
        let Some(fp) = self.ptrace.stop_fp_data.lock().remove(&tid) else {
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
    pub fn restore_current_fp_for_ptrace(&self, tid: TidNumber, _uctx: &mut UserContext) {
        let Some(fp) = self.ptrace.stop_fp_data.lock().remove(&tid) else {
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
    pub fn restore_current_fp_for_ptrace(&self, _tid: TidNumber, _uctx: &mut UserContext) {}

    #[cfg(target_arch = "x86_64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: TidNumber, _uctx: &mut UserContext) {
        let Some(PtraceStopFpData(area)) = self.ptrace.stop_fp_data.lock().remove(&tid) else {
            return;
        };
        unsafe {
            core::arch::x86_64::_fxrstor64((&area as *const ax_cpu::FxsaveArea).cast::<u8>());
        }
    }

    pub fn ptrace_stop_fp_data_for(&self, tid: TidNumber) -> Option<PtraceStopFpData> {
        self.ptrace.stop_fp_data.lock().get(&tid).copied()
    }

    pub fn set_ptrace_stop_fp_data_for(&self, tid: TidNumber, data: PtraceStopFpData) -> bool {
        self.ptrace.stop_fp_data.lock().insert(tid, data).is_some()
    }
}

#[cfg(test)]
mod tests {
    use ax_runtime::hal::cpu::uspace::UserContext;
    use starry_signal::Signo;

    use super::{ProcessPtraceState, PtraceStopKind};
    use crate::{sync::PiMutex, task::TidNumber};

    #[test]
    fn ptrace_heap_registries_use_sleepable_pi_locks() {
        fn assert_pi_mutex<T>(_: &PiMutex<T>) {}
        fn assert_ptrace_lock_types(state: &ProcessPtraceState) {
            assert_pi_mutex(&state.stops);
            assert_pi_mutex(&state.resume_signo);
            assert_pi_mutex(&state.resume_signal_bypass);
            assert_pi_mutex(&state.syscall_trace);
            assert_pi_mutex(&state.pending_event);
            assert_pi_mutex(&state.ss_saved_insn);
            assert_pi_mutex(&state.stop_fp_data);
        }

        let _ = assert_ptrace_lock_types as fn(&ProcessPtraceState);
    }

    #[test]
    fn syscall_stop_is_fully_classified_when_published() {
        let state = ProcessPtraceState::new();
        let tid = TidNumber::try_from(1).unwrap();
        let uctx = UserContext::new(0, 0.into(), 0);

        state.publish_stop(tid, Signo::SIGTRAP, &uctx, PtraceStopKind::syscall(39));

        let stops = state.stops.lock();
        let stop = stops.get(&tid).unwrap();
        assert!(stop.kind.is_syscall());
        #[cfg(target_arch = "x86_64")]
        assert_eq!(stop.kind.syscall_number(), Some(39));
    }
}
