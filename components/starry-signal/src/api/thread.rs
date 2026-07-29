use alloc::sync::Arc;
use core::{
    alloc::Layout,
    mem::{offset_of, size_of},
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
};

use ax_cpu::uspace::UserContext;
use ax_errno::AxResult;
use ax_kspin::SpinNoIrq;
use starry_vm::{VmMutPtr, VmPtr};

use super::ProcessSignalManager;
use crate::{
    DefaultSignalAction, PendingSignals, SignalAction, SignalActionFlags, SignalDisposition,
    SignalInfo, SignalOSAction, SignalSet, SignalStack, Signo, arch::UContext,
};

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct SignalFrame {
    ucontext: UContext,
    siginfo: SignalInfo,
    uctx: UserContext,
    used_sigaltstack: u8,
    _padding: [u8; 15],
}

// SAFETY: every nested context type implements `NoUninit`, the alternate-stack
// flag uses a byte rather than `bool`, and the explicit tail array consumes the
// frame's 16-byte alignment padding.
unsafe impl bytemuck::NoUninit for SignalFrame {}

const _: () = {
    assert!(offset_of!(SignalFrame, ucontext) == 0);
    assert!(offset_of!(SignalFrame, siginfo) == size_of::<UContext>());
    assert!(offset_of!(SignalFrame, uctx) == size_of::<UContext>() + size_of::<SignalInfo>());
    assert!(
        offset_of!(SignalFrame, used_sigaltstack)
            == size_of::<UContext>() + size_of::<SignalInfo>() + size_of::<UserContext>()
    );
    assert!(size_of::<SignalFrame>() == offset_of!(SignalFrame, used_sigaltstack) + 16);
};

enum PreparedSignal {
    Ignore,
    Action(SignalOSAction),
    Handler(PreparedSignalHandler),
}

#[derive(Default)]
struct SigwaitState {
    set: Option<SignalSet>,
    waker: Option<Waker>,
}

struct PreparedSignalHandler {
    signo: Signo,
    siginfo: SignalInfo,
    restore_blocked: SignalSet,
    handler: usize,
    restorer: usize,
    add_blocked: SignalSet,
    use_sigaltstack: bool,
}

/// Thread-level signal manager.
pub struct ThreadSignalManager {
    /// The process-level signal manager
    proc: Arc<ProcessSignalManager>,

    /// The pending signals
    pending: SpinNoIrq<PendingSignals>,
    /// The set of signals currently blocked from delivery.
    blocked: SpinNoIrq<SignalSet>,
    /// The stack used by signal handlers
    stack: SpinNoIrq<SignalStack>,
    /// Number of active signal handlers currently executing on the alternate stack.
    stack_active_depth: SpinNoIrq<usize>,

    possibly_has_signal: AtomicBool,

    /// The synchronous signal-wait state published by `rt_sigtimedwait`.
    ///
    /// The wait set and future waker share one lock so signal delivery observes
    /// a coherent registration. The syscall still rechecks pending signals
    /// after installing the waker, matching Linux's state-publication then
    /// dequeue-again protocol without coupling this component to a scheduler.
    sigwait: SpinNoIrq<SigwaitState>,
}

impl ThreadSignalManager {
    pub fn new(tid: u32, proc: Arc<ProcessSignalManager>) -> Arc<Self> {
        Self::new_with_blocked(tid, proc, SignalSet::default())
    }

    pub fn new_with_blocked(
        tid: u32,
        proc: Arc<ProcessSignalManager>,
        blocked: SignalSet,
    ) -> Arc<Self> {
        let this = Arc::new(Self {
            proc: proc.clone(),

            pending: SpinNoIrq::new(PendingSignals::default()),
            blocked: SpinNoIrq::new(blocked),
            stack: SpinNoIrq::new(SignalStack::default()),
            stack_active_depth: SpinNoIrq::new(0),

            possibly_has_signal: AtomicBool::new(false),
            sigwait: SpinNoIrq::new(SigwaitState::default()),
        });
        proc.children.lock().push((tid, Arc::downgrade(&this)));
        this
    }

    /// Dequeues a signal from the thread's pending signals.
    #[must_use]
    pub fn dequeue_signal(&self, mask: &SignalSet) -> Option<SignalInfo> {
        self.pending
            .lock()
            .dequeue_signal(mask)
            .or_else(|| self.proc.dequeue_signal(mask))
    }

    /// Selects the next signal to deliver, giving a synchronous fault priority
    /// over any other pending signal.
    ///
    /// Mirrors Linux `get_signal` (kernel/signal.c), which calls
    /// `dequeue_synchronous_signal` before the normal `dequeue_signal`: an
    /// instruction-generated fault (`SIGSEGV`/`SIGBUS`/`SIGILL`/`SIGTRAP`/
    /// `SIGFPE`/`SIGSYS` with `si_code > SI_USER`) is delivered ahead of a
    /// concurrently-pending, possibly lower-numbered, asynchronous signal
    /// (e.g. `SIGUSR1`). This is intentionally scoped to the delivery path only;
    /// `dequeue_signal` (used by `rt_sigtimedwait`/`sigwaitinfo`) keeps plain
    /// lowest-numbered ordering, matching Linux where `dequeue_synchronous_signal`
    /// is never called from the sigwait dequeue.
    fn dequeue_deliverable(&self, mask: &SignalSet) -> Option<SignalInfo> {
        if let Some(sig) = self.pending.lock().dequeue_synchronous_signal(mask) {
            return Some(sig);
        }
        if let Some(sig) = self.proc.dequeue_synchronous_signal(mask) {
            return Some(sig);
        }
        if let Some(sig) = self.pending.lock().dequeue_signal(mask) {
            return Some(sig);
        }
        // The thread-level queue is now drained; mirror the fast-path bookkeeping
        // before falling back to the shared process-level queue.
        self.possibly_has_signal.store(false, Ordering::Release);
        self.proc.dequeue_signal(mask)
    }

    pub fn process(&self) -> &Arc<ProcessSignalManager> {
        &self.proc
    }

    /// Publishes the signal set consumed by one synchronous signal wait.
    pub fn begin_sigwait(&self, set: SignalSet) {
        let mut state = self.sigwait.lock();
        debug_assert!(
            state.set.is_none(),
            "one thread cannot own nested synchronous signal waits"
        );
        state.set = Some(set);
        state.waker = None;
    }

    /// Registers the executor waker for the active synchronous signal wait.
    pub fn register_sigwait_waker(&self, waker: &Waker) {
        let mut state = self.sigwait.lock();
        if state.set.is_none() {
            return;
        }
        if state
            .waker
            .as_ref()
            .is_none_or(|current| !current.will_wake(waker))
        {
            state.waker = Some(waker.clone());
        }
    }

    /// Clears the wait set and executor waker after a synchronous wait.
    pub fn finish_sigwait(&self) {
        *self.sigwait.lock() = SigwaitState::default();
    }

    /// Returns whether this thread synchronously waits for `signo`.
    pub fn is_sigwait_for(&self, signo: Signo) -> bool {
        self.sigwait.lock().set.is_some_and(|set| set.has(signo))
    }

    /// Publishes readiness through the registered future waker.
    ///
    /// A direct scheduler wake is insufficient here: if the owner thread is
    /// still running, that wake may be consumed before its local executor
    /// commits to sleep. The future waker carries the executor's sticky
    /// notification bit across that window.
    pub fn wake_sigwait(&self, signo: Signo) {
        let waker = {
            let state = self.sigwait.lock();
            if !state.set.is_some_and(|set| set.has(signo)) {
                return;
            }
            state.waker.clone()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn prepare_signal(
        &self,
        restore_blocked: SignalSet,
        sig: &SignalInfo,
    ) -> (bool, PreparedSignal) {
        let signo = sig.signo();
        debug!("Handle signal: {signo:?}");
        let action = {
            let actions_arc = self.proc.actions();
            let mut actions = actions_arc.lock();
            let action = actions[signo].clone();
            if action.flags.contains(SignalActionFlags::RESETHAND) {
                actions[signo] = SignalAction::default();
            }
            action
        };
        let restartable = action.is_restartable();

        match action.disposition {
            SignalDisposition::Default => (
                restartable,
                match signo.default_action() {
                    DefaultSignalAction::Terminate => {
                        PreparedSignal::Action(SignalOSAction::Terminate)
                    }
                    DefaultSignalAction::CoreDump => {
                        PreparedSignal::Action(SignalOSAction::CoreDump)
                    }
                    DefaultSignalAction::Stop => PreparedSignal::Action(SignalOSAction::Stop),
                    DefaultSignalAction::Ignore => PreparedSignal::Ignore,
                    DefaultSignalAction::Continue => {
                        PreparedSignal::Action(SignalOSAction::Continue)
                    }
                },
            ),
            SignalDisposition::Ignore => (restartable, PreparedSignal::Ignore),
            SignalDisposition::Handler(handler) => {
                let restorer = action
                    .restorer
                    .map_or(self.proc.default_restorer, |f| f as _);
                let mut add_blocked = action.mask;
                if !action.flags.contains(SignalActionFlags::NODEFER) {
                    add_blocked.add(signo);
                }

                (
                    restartable,
                    PreparedSignal::Handler(PreparedSignalHandler {
                        signo,
                        siginfo: *sig,
                        restore_blocked,
                        handler: handler as usize,
                        restorer,
                        add_blocked,
                        use_sigaltstack: action.flags.contains(SignalActionFlags::ONSTACK),
                    }),
                )
            }
        }
    }

    fn install_signal_handler(
        &self,
        uctx: &mut UserContext,
        prepared: PreparedSignalHandler,
    ) -> SignalOSAction {
        let layout = Layout::new::<SignalFrame>();
        let mut uses_sigaltstack = false;
        let sp = if prepared.use_sigaltstack {
            let stack = self.stack.lock();
            if stack.disabled() {
                uctx.sp()
            } else if self.stack_active() {
                uses_sigaltstack = true;
                uctx.sp()
            } else {
                uses_sigaltstack = true;
                stack.sp + stack.size
            }
        } else {
            uctx.sp()
        };
        let aligned_sp = (sp - layout.size()) & !(layout.align() - 1);
        let frame_ptr = aligned_sp as *mut SignalFrame;
        if frame_ptr
            .vm_write(SignalFrame {
                ucontext: UContext::new(uctx, prepared.restore_blocked),
                siginfo: prepared.siginfo,
                uctx: *uctx,
                used_sigaltstack: u8::from(uses_sigaltstack),
                _padding: [0; 15],
            })
            .is_err()
        {
            return SignalOSAction::CoreDump;
        }

        uctx.set_ip(prepared.handler);
        uctx.set_sp(aligned_sp);
        uctx.set_arg0(prepared.signo as _);
        uctx.set_arg1(aligned_sp + offset_of!(SignalFrame, siginfo));
        uctx.set_arg2(aligned_sp + offset_of!(SignalFrame, ucontext));

        #[cfg(target_arch = "x86_64")]
        {
            let new_sp = uctx.sp() - 8;
            if (new_sp as *mut usize).vm_write(prepared.restorer).is_err() {
                return SignalOSAction::CoreDump;
            }
            uctx.set_sp(new_sp);
        }
        #[cfg(not(target_arch = "x86_64"))]
        uctx.set_ra(prepared.restorer);

        *self.blocked.lock() |= prepared.add_blocked;
        if uses_sigaltstack {
            self.enter_stack();
        }
        SignalOSAction::NoFurtherAction
    }

    #[cold]
    fn check_signals_slow_with<F>(
        &self,
        uctx: &mut UserContext,
        restore_blocked: Option<SignalSet>,
        before_deliver: &mut F,
    ) -> Option<(SignalInfo, SignalOSAction)>
    where
        F: FnMut(&mut UserContext, &SignalInfo, bool),
    {
        let blocked = self.blocked.lock();
        let mask = !*blocked;
        let restore_blocked = restore_blocked.unwrap_or_else(|| *blocked);
        drop(blocked);

        loop {
            let sig = self.dequeue_deliverable(&mask)?;
            let (restartable, prepared) = self.prepare_signal(restore_blocked, &sig);
            match prepared {
                PreparedSignal::Ignore => continue,
                PreparedSignal::Action(os_action) => {
                    before_deliver(uctx, &sig, restartable);
                    break Some((sig, os_action));
                }
                PreparedSignal::Handler(prepared) => {
                    before_deliver(uctx, &sig, restartable);
                    let os_action = self.install_signal_handler(uctx, prepared);
                    break Some((sig, os_action));
                }
            }
        }
    }

    /// Checks pending signals and delivers one if possible.
    ///
    /// Calls `before_deliver` immediately before the selected signal is
    /// delivered. The callback receives the user context, the delivered signal,
    /// and whether its disposition is restartable.
    pub fn check_signals_with<F>(
        &self,
        uctx: &mut UserContext,
        restore_blocked: Option<SignalSet>,
        mut before_deliver: F,
    ) -> Option<(SignalInfo, SignalOSAction)>
    where
        F: FnMut(&mut UserContext, &SignalInfo, bool),
    {
        // Fast path
        if !self.possibly_has_signal.load(Ordering::Acquire)
            && !self.proc.possibly_has_signal.load(Ordering::Acquire)
        {
            return None;
        }
        self.check_signals_slow_with(uctx, restore_blocked, &mut before_deliver)
    }

    /// Checks pending signals and delivers one if possible.
    ///
    /// Returns the delivered signal and its delivery result, if any.
    pub fn check_signals(
        &self,
        uctx: &mut UserContext,
        restore_blocked: Option<SignalSet>,
    ) -> Option<(SignalInfo, SignalOSAction)> {
        self.check_signals_with(uctx, restore_blocked, |_, _, _| {})
    }

    /// Restores the signal frame. Called by `sigreturn`.
    pub fn restore(&self, uctx: &mut UserContext) -> AxResult<isize> {
        let frame_ptr = uctx.sp() as *const SignalFrame;
        // copy the saved frame back from uspace
        let frame: SignalFrame = unsafe { frame_ptr.vm_read_uninit()?.assume_init() };

        *uctx = frame.uctx;
        frame.ucontext.mcontext.restore(uctx);

        *self.blocked.lock() = frame.ucontext.sigmask;
        if frame.used_sigaltstack != 0 {
            self.leave_stack();
        }
        self.possibly_has_signal.store(true, Ordering::Release);
        Ok(0)
    }

    /// Sends a signal to the thread.
    ///
    /// Returns `true` if the task was woken up by the signal (i.e. the signal
    /// was not blocked and not ignored).
    ///
    /// See [`ProcessSignalManager::send_signal`] for the process-level version.
    #[must_use]
    pub fn send_signal(&self, sig: SignalInfo) -> bool {
        let signo = sig.signo();

        // Lock by `actions`
        let actions_arc = self.proc.actions();
        let actions = actions_arc.lock();
        debug!("signal: {signo:?}");

        // Skip is_ignore() when the signal is blocked in this thread OR when
        // this thread is inside rt_sigtimedwait/sigwaitinfo waiting for it.
        // POSIX requires that a blocked signal is queued as pending even if
        // its default disposition is to ignore it, so that sigtimedwait() can
        // synchronously consume it.  tgkill/tkill target a specific thread, so
        // we must apply the same exemption here as ProcessSignalManager does
        // for the process-level path.
        let blocked = self.signal_blocked(signo);
        let in_sigwait = self.is_sigwait_for(signo);
        if !blocked && !in_sigwait && actions[signo].is_ignore(signo) {
            return false;
        }

        if self.pending.lock().put_signal(sig) {
            self.possibly_has_signal.store(true, Ordering::Release);
        }
        let deliverable = !self.signal_blocked(signo);
        drop(actions);
        // The sigwait future is owned by this signal manager. Publish pending
        // state before invoking the task-context waker and never wake while an
        // action or pending-signal lock remains held.
        self.wake_sigwait(signo);
        deliverable
    }

    /// Gets the blocked signals.
    pub fn blocked(&self) -> SignalSet {
        *self.blocked.lock()
    }

    /// Sets the blocked signals. Return the old value.
    pub fn set_blocked(&self, mut set: SignalSet) -> SignalSet {
        // Lock by `actions`
        let actions_arc = self.proc.actions();
        let _actions = actions_arc.lock();

        set.remove(Signo::SIGKILL);
        set.remove(Signo::SIGSTOP);
        self.possibly_has_signal.store(true, Ordering::Release);
        let mut guard = self.blocked.lock();
        let old = *guard;
        *guard = set;
        old
    }

    /// Checks if a signal is blocked.
    pub fn signal_blocked(&self, signo: Signo) -> bool {
        self.blocked.lock().has(signo)
    }

    /// Gets the signal stack.
    pub fn stack(&self) -> SignalStack {
        let stack = *self.stack.lock();
        if self.stack_active() {
            stack.on_stack()
        } else {
            stack
        }
    }

    /// Sets the signal stack.
    pub fn set_stack(&self, stack: SignalStack) {
        *self.stack.lock() = stack.without_runtime_flags();
    }

    pub fn stack_active(&self) -> bool {
        *self.stack_active_depth.lock() > 0
    }

    fn enter_stack(&self) {
        *self.stack_active_depth.lock() += 1;
    }

    fn leave_stack(&self) {
        let mut depth = self.stack_active_depth.lock();
        *depth = depth.saturating_sub(1);
    }

    /// Gets current pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock().set | self.proc.pending()
    }

    /// Resets the alternate signal stack to the default (disabled, addr=0)
    /// across `execve`. The pre-exec stack address pointed into user
    /// memory that no longer exists once the new aspace replaces the old.
    pub fn reset_stack(&self) {
        *self.stack.lock() = SignalStack::default();
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, task::Wake};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::api::SignalActions;

    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn sigwait_waker_only_fires_for_the_published_set() {
        let actions = Arc::new(SpinNoIrq::new(SignalActions::default()));
        let process = Arc::new(ProcessSignalManager::new(actions, 0));
        let thread = ThreadSignalManager::new(1, process);
        let counter = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(counter.clone());
        let mut set = SignalSet::default();
        set.add(Signo::SIGCHLD);

        thread.begin_sigwait(set);
        thread.register_sigwait_waker(&waker);
        thread.wake_sigwait(Signo::SIGURG);
        assert_eq!(counter.0.load(Ordering::Relaxed), 0);

        let _deliverable = thread.send_signal(SignalInfo::new_kernel(Signo::SIGCHLD));
        assert_eq!(counter.0.load(Ordering::Relaxed), 1);

        thread.finish_sigwait();
        thread.wake_sigwait(Signo::SIGCHLD);
        assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    }
}
