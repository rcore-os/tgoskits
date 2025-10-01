use core::{
    alloc::Layout,
    mem::offset_of,
    sync::atomic::{AtomicBool, Ordering},
};

use alloc::sync::Arc;
use axcpu::uspace::UserContext;
use kspin::SpinNoIrq;
use starry_vm::VmMutPtr;

use crate::{
    DefaultSignalAction, PendingSignals, SignalAction, SignalActionFlags, SignalDisposition,
    SignalInfo, SignalOSAction, SignalSet, SignalStack, Signo, arch::UContext,
};

use super::ProcessSignalManager;

struct SignalFrame {
    ucontext: UContext,
    siginfo: SignalInfo,
    uctx: UserContext,
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

    pub fn handle_signal(
        &self,
        uctx: &mut UserContext,
        restore_blocked: SignalSet,
        sig: &SignalInfo,
        action: &SignalAction,
    ) -> Option<SignalOSAction> {
        let signo = sig.signo();
        debug!("Handle signal: {signo:?}");
        match action.disposition {
            SignalDisposition::Default => match signo.default_action() {
                DefaultSignalAction::Terminate => Some(SignalOSAction::Terminate),
                DefaultSignalAction::CoreDump => Some(SignalOSAction::CoreDump),
                DefaultSignalAction::Stop => Some(SignalOSAction::Stop),
                DefaultSignalAction::Ignore => None,
                DefaultSignalAction::Continue => Some(SignalOSAction::Continue),
            },
            SignalDisposition::Ignore => None,
            SignalDisposition::Handler(handler) => {
                let layout = Layout::new::<SignalFrame>();
                let stack = self.stack.lock();
                let sp = if stack.disabled() || !action.flags.contains(SignalActionFlags::ONSTACK) {
                    uctx.sp()
                } else {
                    stack.sp + stack.size
                };
                drop(stack);

                let aligned_sp = (sp - layout.size()) & !(layout.align() - 1);

                let frame_ptr = aligned_sp as *mut SignalFrame;
                if frame_ptr
                    .vm_write(SignalFrame {
                        ucontext: UContext::new(uctx, restore_blocked),
                        siginfo: sig.clone(),
                        uctx: *uctx,
                    })
                    .is_err()
                {
                    return Some(SignalOSAction::CoreDump);
                }

                uctx.set_ip(handler as usize);
                uctx.set_sp(aligned_sp);
                uctx.set_arg0(signo as _);
                uctx.set_arg1(aligned_sp + offset_of!(SignalFrame, siginfo));
                uctx.set_arg2(aligned_sp + offset_of!(SignalFrame, ucontext));

                let restorer = action
                    .restorer
                    .map_or(self.proc.default_restorer, |f| f as _);
                #[cfg(target_arch = "x86_64")]
                if (uctx.sp() as *mut usize).vm_write(restorer).is_err() {
                    return Some(SignalOSAction::CoreDump);
                }
                #[cfg(not(target_arch = "x86_64"))]
                uctx.set_ra(restorer);

                let mut add_blocked = action.mask;
                if !action.flags.contains(SignalActionFlags::NODEFER) {
                    add_blocked.add(signo);
                }

                if action.flags.contains(SignalActionFlags::RESETHAND) {
                    self.proc.actions.lock()[signo] = SignalAction::default();
                }
                *self.blocked.lock() |= add_blocked;
                Some(SignalOSAction::Handler)
            }
        }
    }

    #[cold]
    fn check_signals_slow(
        &self,
        uctx: &mut UserContext,
        restore_blocked: Option<SignalSet>,
    ) -> Option<(SignalInfo, SignalOSAction)> {
        let actions = self.proc.actions.lock();

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
            let action = &actions[sig.signo()];
            if let Some(os_action) = self.handle_signal(uctx, restore_blocked, &sig, action) {
                break Some((sig, os_action));
            }
        }
    }

    /// Checks pending signals and handle them.
    ///
    /// Returns the signal number and the action the OS should take, if any.
    pub fn check_signals(
        &self,
        uctx: &mut UserContext,
        restore_blocked: Option<SignalSet>,
    ) -> Option<(SignalInfo, SignalOSAction)> {
        // Fast path
        if !self.possibly_has_signal.load(Ordering::Acquire)
            && !self.proc.possibly_has_signal.load(Ordering::Acquire)
        {
            return None;
        }
        self.check_signals_slow(uctx, restore_blocked)
    }

    /// Restores the signal frame. Called by `sigreturn`.
    pub fn restore(&self, uctx: &mut UserContext) {
        let frame_ptr = uctx.sp() as *const SignalFrame;
        // SAFETY: pointer is valid
        let frame = unsafe { &*frame_ptr };

        *uctx = frame.uctx;
        frame.ucontext.mcontext.restore(uctx);

        *self.blocked.lock() = frame.ucontext.sigmask;
        self.possibly_has_signal.store(true, Ordering::Release);
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
        if self.proc.signal_ignored(signo) {
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
