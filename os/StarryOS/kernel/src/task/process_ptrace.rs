//! Ptrace ownership, stop records, and architecture register snapshots.

use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use ax_runtime::hal::cpu::uspace::UserContext;
use ax_sync::spin::SpinNoIrq;
use axpoll::{IoEvents, PollSet};
use starry_signal::{SignalInfo, Signo};

use super::ProcessData;

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

/// Ptrace state owned by one process generation.
pub(super) struct ProcessPtraceState {
    tracer_pid: AtomicU32,
    traceme: AtomicBool,
    stops: SpinNoIrq<BTreeMap<u32, PtraceStopRecord>>,
    selected_tid: AtomicU32,
    stop_event: Arc<PollSet>,
    resume_signo: SpinNoIrq<BTreeMap<u32, u32>>,
    resume_signal_bypass: SpinNoIrq<BTreeMap<u32, u32>>,
    exec_stop_pending: AtomicBool,
    attached: AtomicBool,
    singlestep_tid: AtomicU32,
    syscall_trace: SpinNoIrq<BTreeMap<u32, SyscallTraceState>>,
    options: AtomicUsize,
    pending_event: SpinNoIrq<BTreeMap<u32, PtracePendingEvent>>,
    ss_saved_insn: SpinNoIrq<BTreeMap<u32, (usize, usize)>>,
    stop_fp_data: SpinNoIrq<BTreeMap<u32, PtraceStopFpData>>,
}

impl ProcessPtraceState {
    pub(super) fn new() -> Self {
        Self {
            tracer_pid: AtomicU32::new(0),
            traceme: AtomicBool::new(false),
            stops: SpinNoIrq::new(BTreeMap::new()),
            selected_tid: AtomicU32::new(0),
            stop_event: Arc::default(),
            resume_signo: SpinNoIrq::new(BTreeMap::new()),
            resume_signal_bypass: SpinNoIrq::new(BTreeMap::new()),
            exec_stop_pending: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            singlestep_tid: AtomicU32::new(0),
            syscall_trace: SpinNoIrq::new(BTreeMap::new()),
            options: AtomicUsize::new(0),
            pending_event: SpinNoIrq::new(BTreeMap::new()),
            ss_saved_insn: SpinNoIrq::new(BTreeMap::new()),
            stop_fp_data: SpinNoIrq::new(BTreeMap::new()),
        }
    }
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
            self.set_ptrace_tracer_pid(parent.pid());
        }
        self.ptrace.traceme.store(true, Ordering::Release);
    }

    pub fn clear_ptrace_traceme(&self) {
        self.ptrace.traceme.store(false, Ordering::Release);
    }

    pub fn is_ptrace_traceme(&self) -> bool {
        self.ptrace.traceme.load(Ordering::Acquire)
    }

    pub fn set_ptrace_tracer_pid(&self, pid: starry_process::Pid) {
        self.ptrace.tracer_pid.store(pid, Ordering::Release);
    }

    pub fn clear_ptrace_tracer_pid(&self) {
        self.ptrace.tracer_pid.store(0, Ordering::Release);
    }

    pub fn ptrace_tracer_pid(&self) -> Option<starry_process::Pid> {
        let pid = self.ptrace.tracer_pid.load(Ordering::Acquire);
        if pid == 0 { None } else { Some(pid) }
    }

    /// Record that this tracee is stopped by `signo`.
    pub fn set_ptrace_stop(&self, tid: u32, signo: Signo, uctx: &UserContext) {
        let pending_event = self.ptrace.pending_event.lock().remove(&tid);
        self.ptrace.stops.lock().insert(
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
        self.ptrace.selected_tid.store(tid, Ordering::Release);
    }

    /// Record that this tracee is stopped at a syscall entry or exit boundary.
    pub fn set_ptrace_syscall_stop(&self, tid: u32, signo: Signo, uctx: &UserContext) {
        self.set_ptrace_stop(tid, signo, uctx);
        if let Some(stop) = self.ptrace.stops.lock().get_mut(&tid) {
            stop.is_syscall = true;
        }
    }

    pub fn ptrace_stop_tid(&self) -> Option<u32> {
        let stops = self.ptrace.stops.lock();
        stops
            .iter()
            .find_map(|(tid, stop)| (!stop.reported && stop.signo.is_some()).then_some(*tid))
            .or_else(|| stops.keys().next().copied())
    }

    pub fn select_ptrace_stop(&self, tid: u32) -> bool {
        if self.ptrace.stops.lock().contains_key(&tid) {
            self.ptrace.selected_tid.store(tid, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub fn selected_ptrace_stop_tid(&self) -> Option<u32> {
        let selected = self.ptrace.selected_tid.load(Ordering::Acquire);
        let stops = self.ptrace.stops.lock();
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
        self.ptrace.stops.lock().contains_key(&tid)
    }

    pub fn ptrace_stop_signo_for(&self, tid: u32) -> Option<Signo> {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .and_then(|stop| stop.signo)
    }

    pub fn ptrace_unreported_stop(&self, preferred_tid: Option<u32>) -> Option<(u32, Signo)> {
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

    pub fn ptrace_unreported_stop_for(&self, tid: u32) -> Option<(u32, Signo)> {
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

    pub fn is_ptrace_syscall_stop_for(&self, tid: u32) -> bool {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .is_some_and(|stop| stop.is_syscall)
    }

    pub fn ptrace_stop_siginfo_for(&self, tid: u32) -> Option<SignalInfo> {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .and_then(|stop| stop.siginfo)
    }

    pub fn set_ptrace_stop_siginfo_for(&self, tid: u32, signo: Signo, siginfo: SignalInfo) -> bool {
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

    pub fn claim_ptrace_stop(&self, tid: u32) -> bool {
        !self.ptrace.stops.lock().contains_key(&tid)
    }

    pub fn ptrace_stop_user_context_for(&self, tid: u32) -> Option<UserContext> {
        self.ptrace.stops.lock().get(&tid).map(|stop| stop.uctx)
    }

    pub fn mark_ptrace_stop_reported_for(&self, tid: u32) {
        if let Some(stop) = self.ptrace.stops.lock().get_mut(&tid) {
            stop.reported = true;
        }
    }

    pub fn set_ptrace_stop_user_context_for(&self, tid: u32, uctx: UserContext) -> bool {
        let mut stops = self.ptrace.stops.lock();
        let Some(stop) = stops.get_mut(&tid) else {
            return false;
        };
        stop.uctx = uctx;
        true
    }

    pub fn resume_ptrace_stop_with_signal_for(&self, tid: u32, signo: u32) {
        if let Some(stop) = self.ptrace.stops.lock().get_mut(&tid) {
            self.ptrace.resume_signo.lock().insert(tid, signo);
            stop.signo = None;
            stop.siginfo = None;
            stop.is_syscall = false;
            stop.reported = false;
            stop.event = 0;
            stop.event_msg = 0;
        }
        // Ptrace stop state is updated before waking waiters.
        unsafe { self.ptrace.stop_event.wake(IoEvents::IN) };
    }

    /// Consume the signal chosen by the tracer on resume.
    pub fn take_ptrace_resume_signo_for(&self, tid: u32) -> Option<Signo> {
        let signo = self.ptrace.resume_signo.lock().remove(&tid).unwrap_or(0);
        Signo::from_repr(signo as u8)
    }

    pub fn set_ptrace_resume_signal_bypass_for(&self, tid: u32, signo: Signo) {
        self.ptrace
            .resume_signal_bypass
            .lock()
            .insert(tid, signo as u32);
    }

    pub fn take_ptrace_resume_signal_bypass_for(&self, tid: u32, signo: Signo) -> bool {
        let mut bypass = self.ptrace.resume_signal_bypass.lock();
        if bypass.get(&tid).copied() == Some(signo as u32) {
            bypass.remove(&tid);
            true
        } else {
            false
        }
    }

    pub fn take_ptrace_stop_user_context_for(&self, tid: u32) -> Option<UserContext> {
        let uctx = self.ptrace.stops.lock().remove(&tid).map(|stop| stop.uctx);
        if uctx.is_some() && self.ptrace.selected_tid.load(Ordering::Acquire) == tid {
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

    pub fn set_ptrace_exec_stop_pending(&self) {
        self.ptrace
            .exec_stop_pending
            .store(true, core::sync::atomic::Ordering::Release);
    }

    pub fn take_ptrace_exec_stop_pending(&self) -> bool {
        self.ptrace
            .exec_stop_pending
            .swap(false, core::sync::atomic::Ordering::AcqRel)
    }

    /// Register a waiter for changes to this process's ptrace stop state.
    pub fn register_ptrace_stop_waker(&self, waker: &core::task::Waker) {
        // Registration happens from task/wait context.
        unsafe { self.ptrace.stop_event.register(waker, IoEvents::IN) };
    }

    pub fn set_ptrace_attached(&self) {
        self.ptrace.attached.store(true, Ordering::Release);
    }

    pub fn clear_ptrace_attached(&self) {
        self.ptrace.attached.store(false, Ordering::Release);
    }

    pub fn is_ptrace_attached(&self) -> bool {
        self.ptrace.attached.load(Ordering::Acquire)
    }

    pub fn set_ptrace_singlestep_for(&self, tid: u32, val: bool) {
        self.ptrace
            .singlestep_tid
            .store(if val { tid } else { 0 }, Ordering::Release);
    }

    pub fn is_ptrace_singlestep_for(&self, tid: u32) -> bool {
        self.ptrace.singlestep_tid.load(Ordering::Acquire) == tid
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
        let mut traces = self.ptrace.syscall_trace.lock();
        if matches!(state, SyscallTraceState::None) {
            traces.remove(&tid);
        } else {
            traces.insert(tid, state);
        }
    }

    pub fn take_ptrace_syscall_trace_for(&self, tid: u32) -> SyscallTraceState {
        self.ptrace
            .syscall_trace
            .lock()
            .remove(&tid)
            .unwrap_or_default()
    }

    pub fn set_ptrace_options(&self, opts: usize) {
        self.ptrace.options.store(opts, Ordering::Release);
    }

    pub fn ptrace_options(&self) -> usize {
        self.ptrace.options.load(Ordering::Acquire)
    }

    pub fn ptrace_event_msg_for(&self, tid: u32) -> usize {
        self.ptrace
            .stops
            .lock()
            .get(&tid)
            .map_or(0, |stop| stop.event_msg)
    }

    pub fn set_ptrace_pending_event(&self, tid: u32, event: u32, msg: usize) {
        self.ptrace
            .pending_event
            .lock()
            .insert(tid, PtracePendingEvent { event, msg });
    }

    pub fn has_ptrace_pending_event_for(&self, tid: u32) -> bool {
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

    pub fn ptrace_event_for(&self, tid: u32) -> Option<u32> {
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
    pub fn set_ptrace_ss_saved_insn_for(&self, tid: u32, saved: Option<(usize, usize)>) {
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
    pub fn take_ptrace_ss_saved_insn_for(&self, tid: u32) -> Option<(usize, usize)> {
        self.ptrace.ss_saved_insn.lock().remove(&tid)
    }

    #[cfg(target_arch = "riscv64")]
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
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
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
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
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
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
    pub fn save_current_fp_for_ptrace(&self, _tid: u32) {}

    #[cfg(target_arch = "x86_64")]
    pub fn save_current_fp_for_ptrace(&self, tid: u32) {
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
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, uctx: &mut UserContext) {
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
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, _uctx: &mut UserContext) {
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
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, _uctx: &mut UserContext) {
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
    pub fn restore_current_fp_for_ptrace(&self, _tid: u32, _uctx: &mut UserContext) {}

    #[cfg(target_arch = "x86_64")]
    pub fn restore_current_fp_for_ptrace(&self, tid: u32, _uctx: &mut UserContext) {
        let Some(PtraceStopFpData(area)) = self.ptrace.stop_fp_data.lock().remove(&tid) else {
            return;
        };
        unsafe {
            core::arch::x86_64::_fxrstor64((&area as *const ax_cpu::FxsaveArea).cast::<u8>());
        }
    }

    pub fn ptrace_stop_fp_data_for(&self, tid: u32) -> Option<PtraceStopFpData> {
        self.ptrace.stop_fp_data.lock().get(&tid).copied()
    }

    pub fn set_ptrace_stop_fp_data_for(&self, tid: u32, data: PtraceStopFpData) -> bool {
        self.ptrace.stop_fp_data.lock().insert(tid, data).is_some()
    }
}
