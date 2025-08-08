use core::{
    array,
    ops::{Index, IndexMut},
    sync::atomic::{AtomicBool, Ordering},
};

use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use kspin::SpinNoIrq;

use crate::{
    DefaultSignalAction, PendingSignals, SignalAction, SignalActionFlags, SignalDisposition,
    SignalInfo, SignalSet, Signo, api::ThreadSignalManager,
};

/// Signal actions for a process.
#[derive(Clone)]
pub struct SignalActions(pub(crate) [SignalAction; 64]);

impl Default for SignalActions {
    fn default() -> Self {
        Self(array::from_fn(|_| SignalAction::default()))
    }
}

impl Index<Signo> for SignalActions {
    type Output = SignalAction;
    fn index(&self, signo: Signo) -> &SignalAction {
        &self.0[signo as usize - 1]
    }
}

impl IndexMut<Signo> for SignalActions {
    fn index_mut(&mut self, signo: Signo) -> &mut SignalAction {
        &mut self.0[signo as usize - 1]
    }
}

/// Process-level signal manager.
pub struct ProcessSignalManager {
    /// The process-level shared pending signals
    pending: SpinNoIrq<PendingSignals>,

    /// The signal actions
    pub actions: Arc<SpinNoIrq<SignalActions>>,

    /// The default restorer function.
    pub(crate) default_restorer: usize,

    /// Thread-level signal managers.
    pub(crate) children: SpinNoIrq<Vec<(u32, Weak<ThreadSignalManager>)>>,

    pub(crate) possibly_has_signal: AtomicBool,
}

impl ProcessSignalManager {
    /// Creates a new process signal manager.
    pub fn new(actions: Arc<SpinNoIrq<SignalActions>>, default_restorer: usize) -> Self {
        Self {
            pending: SpinNoIrq::new(PendingSignals::default()),
            actions,
            default_restorer,
            children: SpinNoIrq::new(Vec::new()),
            possibly_has_signal: AtomicBool::new(false),
        }
    }

    pub(crate) fn dequeue_signal(&self, mask: &SignalSet) -> Option<SignalInfo> {
        self.pending.lock().dequeue_signal(mask)
    }

    /// Checks if a signal is ignored by the process.
    pub fn signal_ignored(&self, signo: Signo) -> bool {
        match &self.actions.lock()[signo].disposition {
            SignalDisposition::Ignore => true,
            SignalDisposition::Default => {
                matches!(signo.default_action(), DefaultSignalAction::Ignore)
            }
            _ => false,
        }
    }

    /// Checks if syscalls interrupted by the given signal can be restarted.
    pub fn can_restart(&self, signo: Signo) -> bool {
        self.actions.lock()[signo]
            .flags
            .contains(SignalActionFlags::RESTART)
    }

    /// Sends a signal to the process.
    ///
    /// Returns `Some(tid)` if the signal wakes up a thread.
    ///
    /// See [`ThreadSignalManager::send_signal`] for the thread-level version.
    #[must_use]
    pub fn send_signal(&self, sig: SignalInfo) -> Option<u32> {
        let signo = sig.signo();
        if self.signal_ignored(signo) {
            return None;
        }

        if self.pending.lock().put_signal(sig) {
            self.possibly_has_signal.store(true, Ordering::Release);
        }
        let mut result = None;
        self.children.lock().retain(|(tid, thread)| {
            if let Some(thread) = thread.upgrade() {
                if result.is_none() && !thread.signal_blocked(signo) {
                    result = Some(*tid);
                }
                true
            } else {
                false
            }
        });
        result
    }

    /// Gets currently pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock().set
    }
}
