use core::{
    array,
    ops::{Index, IndexMut},
};

use alloc::sync::Arc;
use event_listener::{Event, listener};
use kspin::SpinNoIrq;

use crate::{PendingSignals, SignalAction, SignalInfo, SignalSet, Signo};

/// Signal actions for a process.
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

    pub(crate) event: Event,

    /// The default restorer function.
    pub(crate) default_restorer: usize,
}

impl ProcessSignalManager {
    /// Creates a new process signal manager.
    pub fn new(actions: Arc<SpinNoIrq<SignalActions>>, default_restorer: usize) -> Self {
        Self {
            pending: SpinNoIrq::new(PendingSignals::default()),
            actions,
            event: Event::new(),
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
        self.event.notify(1);
    }

    /// Gets currently pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock().set
    }

    /// Wait until a signal is delivered to this process.
    pub async fn wait(&self) {
        listener!(self.event => listener);
        listener.await
    }
}
