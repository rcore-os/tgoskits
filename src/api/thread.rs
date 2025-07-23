use core::{alloc::Layout, mem::offset_of};

use alloc::sync::Arc;
use axcpu::TrapFrame;
use event_listener::listener;
use kspin::SpinNoIrq;
use starry_vm::VmMutPtr;

use crate::{
    DefaultSignalAction, PendingSignals, SignalAction, SignalActionFlags, SignalDisposition,
    SignalInfo, SignalOSAction, SignalSet, SignalStack, arch::UContext,
};

use super::ProcessSignalManager;

struct SignalFrame {
    ucontext: UContext,
    siginfo: SignalInfo,
    tf: TrapFrame,
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
}

impl ThreadSignalManager {
    pub fn new(proc: Arc<ProcessSignalManager>) -> Self {
        Self {
            proc,
            pending: SpinNoIrq::new(PendingSignals::default()),
            blocked: SpinNoIrq::new(SignalSet::default()),
            stack: SpinNoIrq::new(SignalStack::default()),
        }
    }

    fn dequeue_signal(&self, mask: &SignalSet) -> Option<SignalInfo> {
        self.pending
            .lock()
            .dequeue_signal(mask)
            .or_else(|| self.proc.dequeue_signal(mask))
    }

    fn handle_signal(
        &self,
        tf: &mut TrapFrame,
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
                    tf.sp()
                } else {
                    stack.sp
                };
                drop(stack);

                let aligned_sp = (sp - layout.size()) & !(layout.align() - 1);

                let frame_ptr = aligned_sp as *mut SignalFrame;
                if frame_ptr
                    .vm_write(SignalFrame {
                        ucontext: UContext::new(tf, restore_blocked),
                        siginfo: sig.clone(),
                        tf: *tf,
                    })
                    .is_err()
                {
                    return Some(SignalOSAction::CoreDump);
                }

                tf.set_ip(handler as usize);
                tf.set_sp(aligned_sp);
                tf.set_arg0(signo as _);
                tf.set_arg1(aligned_sp + offset_of!(SignalFrame, siginfo));
                tf.set_arg2(aligned_sp + offset_of!(SignalFrame, ucontext));

                let restorer = action
                    .restorer
                    .map_or(self.proc.default_restorer, |f| f as _);
                // FIXME: fix x86_64 RA handling
                #[cfg(target_arch = "x86_64")]
                tf.push_ra(restorer);
                #[cfg(not(target_arch = "x86_64"))]
                tf.set_ra(restorer);

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

    /// Checks pending signals and handle them.
    ///
    /// Returns the signal number and the action the OS should take, if any.
    pub fn check_signals(
        &self,
        tf: &mut TrapFrame,
        restore_blocked: Option<SignalSet>,
    ) -> Option<(SignalInfo, SignalOSAction)> {
        let actions = self.proc.actions.lock();

        let blocked = self.blocked.lock();
        let mask = !*blocked;
        let restore_blocked = restore_blocked.unwrap_or_else(|| *blocked);
        drop(blocked);

        loop {
            let sig = self.dequeue_signal(&mask)?;
            let action = &actions[sig.signo()];
            if let Some(os_action) = self.handle_signal(tf, restore_blocked, &sig, action) {
                break Some((sig, os_action));
            }
        }
    }

    /// Restores the signal frame. Called by `sigreturn`.
    pub fn restore(&self, tf: &mut TrapFrame) {
        let frame_ptr = tf.sp() as *const SignalFrame;
        // SAFETY: pointer is valid
        let frame = unsafe { &*frame_ptr };

        *tf = frame.tf;
        frame.ucontext.mcontext.restore(tf);

        *self.blocked.lock() = frame.ucontext.sigmask;
    }

    /// Sends a signal to the thread.
    ///
    /// See [`ProcessSignalManager::send_signal`] for the process-level version.
    pub fn send_signal(&self, sig: SignalInfo) {
        self.pending.lock().put_signal(sig);
        self.proc.event.notify(1);
    }

    /// Gets the blocked signals.
    pub fn blocked(&self) -> SignalSet {
        *self.blocked.lock()
    }

    /// Applies a function to the blocked signals.
    pub fn with_blocked_mut<R>(&self, f: impl FnOnce(&mut SignalSet) -> R) -> R {
        f(&mut self.blocked.lock())
    }

    /// Gets the signal stack.
    pub fn stack(&self) -> SignalStack {
        self.stack.lock().clone()
    }

    /// Applies a function to the signal stack.
    pub fn with_stack_mut<R>(&self, f: impl FnOnce(&mut SignalStack) -> R) -> R {
        f(&mut self.stack.lock())
    }

    /// Gets current pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock().set | self.proc.pending()
    }

    /// Wait until one of the signals in `set` is pending.
    ///
    /// If one of the signals in set is already pending for the calling thread,
    /// this will return immediately.
    ///
    /// Returns the signal that was received.
    pub async fn wait(&self, mut set: SignalSet) -> SignalInfo {
        // Non-blocked signals cannot be waited
        set &= self.blocked();

        loop {
            if let Some(sig) = self.dequeue_signal(&set) {
                return sig;
            }

            listener!(self.proc.event => listener);

            if let Some(sig) = self.dequeue_signal(&set) {
                return sig;
            }

            listener.await;
        }
    }
}
