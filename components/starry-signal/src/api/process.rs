use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    array,
    ops::{Index, IndexMut},
    sync::atomic::{AtomicBool, Ordering},
};

use ax_errno::AxResult;
use ax_kspin::SpinNoIrq;
use linux_raw_sys::general::kernel_sigaction;
use starry_vm::{VmMutPtr, VmPtr};

use crate::{PendingSignals, SignalAction, SignalInfo, SignalSet, Signo, api::ThreadSignalManager};

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
        let mut guard = self.pending.lock();
        let result = guard.dequeue_signal(mask);
        if guard.set.is_empty() {
            self.possibly_has_signal.store(false, Ordering::Release);
        }
        result
    }

    /// Sends a signal to the process.
    ///
    /// Returns `Some(tid)` if the signal wakes up a thread.
    ///
    /// See [`ThreadSignalManager::send_signal`] for the thread-level version.
    #[must_use]
    pub fn send_signal(&self, sig: SignalInfo) -> Option<u32> {
        let signo = sig.signo();

        // Lock by `actions`
        let actions = self.actions.lock();
        if actions[signo].is_ignore(signo) {
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

    /// Resets actions to empty.
    pub fn reset_actions(&self) {
        *self.actions.lock() = Default::default();
    }

    /// Registers a new action and returns the old one.
    pub fn set_action(
        &self,
        signo: Signo,
        act: *const kernel_sigaction,
        oldact: *mut kernel_sigaction,
    ) -> AxResult<isize> {
        let new_action = if let Some(act) = act.nullable() {
            let act = unsafe { act.vm_read_uninit()?.assume_init() }.into();
            debug!("sys_rt_sigaction <= signo: {signo:?}, act: {act:?}");
            Some(act)
        } else {
            None
        };

        let old_action = {
            let mut actions = self.actions.lock();
            let old = actions[signo].clone();
            if let Some(act) = new_action {
                actions[signo] = act;
            }
            old
        };

        if let Some(oldact) = oldact.nullable() {
            oldact.vm_write(old_action.into())?;
        }
        Ok(0)
    }
}
