use core::array;

use lock_api::{Mutex, RawMutex};

use crate::{PendingSignals, SignalAction, SignalInfo, SignalSet, Signo};

use super::WaitQueue;

/// Process-level signal manager.
pub struct ProcessSignalManager<M, WQ> {
    /// The process-level shared pending signals
    pending: Mutex<M, PendingSignals>,
    /// The signal actions
    pub(crate) signal_actions: Mutex<M, [SignalAction; 64]>,
    /// The wait queue for signal. Used by `rt_sigtimedwait`, etc.
    ///
    /// Note that this is shared by all threads in the process, so false wakeups
    /// may occur.
    pub(crate) signal_wq: WQ,

    /// The default restorer function.
    pub(crate) default_restorer: usize,
}
impl<M: RawMutex, WQ: WaitQueue> ProcessSignalManager<M, WQ> {
    /// Creates a new process signal manager given the default restorer function
    /// address.
    pub fn new(default_restorer: usize) -> Self {
        Self {
            pending: Mutex::new(PendingSignals::new()),
            signal_actions: Mutex::new(array::from_fn(|_| SignalAction::default())),
            signal_wq: WQ::default(),
            default_restorer,
        }
    }

    pub(crate) fn dequeue_signal(&self, mask: &SignalSet) -> Option<SignalInfo> {
        self.pending.lock().dequeue_signal(mask)
    }

    /// Sends a signal to the process.
    ///
    /// See [`ThreadSignalManager::send_signal`] for the thread-level version.
    pub fn send_signal(&self, sig: SignalInfo) {
        self.pending.lock().put_signal(sig);
        self.signal_wq.notify_one();
    }

    /// Applies a function to the signal action.
    pub fn with_action_mut<R>(&self, signo: Signo, f: impl FnOnce(&mut SignalAction) -> R) -> R {
        f(&mut self.signal_actions.lock()[signo as usize - 1])
    }

    /// Gets currently pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock().set
    }
}
