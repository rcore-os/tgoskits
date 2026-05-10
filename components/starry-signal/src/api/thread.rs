use alloc::sync::Arc;
use core::{
    alloc::Layout,
    mem::offset_of,
    sync::atomic::{AtomicBool, Ordering},
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

struct SignalFrame {
    ucontext: UContext,
    siginfo: SignalInfo,
    uctx: UserContext,
}

enum PreparedSignal {
    Ignore,
    Action(SignalOSAction),
    Handler(PreparedSignalHandler),
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

    possibly_has_signal: AtomicBool,
}

impl ThreadSignalManager {
    pub fn new(tid: u32, proc: Arc<ProcessSignalManager>) -> Arc<Self> {
        let this = Arc::new(Self {
            proc: proc.clone(),

            pending: SpinNoIrq::new(PendingSignals::default()),
            blocked: SpinNoIrq::new(SignalSet::default()),
            stack: SpinNoIrq::new(SignalStack::default()),

            possibly_has_signal: AtomicBool::new(false),
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

    pub fn process(&self) -> &Arc<ProcessSignalManager> {
        &self.proc
    }

    fn prepare_signal(
        &self,
        restore_blocked: SignalSet,
        sig: &SignalInfo,
    ) -> (bool, PreparedSignal) {
        let signo = sig.signo();
        debug!("Handle signal: {signo:?}");
        let action = {
            let mut actions = self.proc.actions.lock();
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
                        siginfo: sig.clone(),
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
        let sp = if prepared.use_sigaltstack {
            let stack = self.stack.lock();
            if stack.disabled() {
                uctx.sp()
            } else {
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
            let sig = match self.pending.lock().dequeue_signal(&mask) {
                Some(sig) => Some(sig),
                None => {
                    self.possibly_has_signal.store(false, Ordering::Release);
                    self.proc.dequeue_signal(&mask)
                }
            }?;
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
        let actions = self.proc.actions.lock();
        debug!("signal: {signo:?}");
        if actions[signo].is_ignore(signo) {
            return false;
        }

        if self.pending.lock().put_signal(sig) {
            self.possibly_has_signal.store(true, Ordering::Release);
        }
        !self.signal_blocked(signo)
    }

    /// Gets the blocked signals.
    pub fn blocked(&self) -> SignalSet {
        *self.blocked.lock()
    }

    /// Sets the blocked signals. Return the old value.
    pub fn set_blocked(&self, mut set: SignalSet) -> SignalSet {
        // Lock by `actions`
        let _actions = self.proc.actions.lock();

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
        self.stack.lock().clone()
    }

    /// Sets the signal stack.
    pub fn set_stack(&self, stack: SignalStack) {
        *self.stack.lock() = stack;
    }

    /// Gets current pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock().set | self.proc.pending()
    }
}
