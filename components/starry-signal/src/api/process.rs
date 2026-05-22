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

    /// The signal actions. Held in a swappable slot because `CLONE_SIGHAND`
    /// hands the inner `Arc` to a peer process; `execve` must be able to
    /// detach this manager from that shared inner table (to reset handlers
    /// for the new image) without mutating the table the peer still uses.
    /// Outside of exec, callers should obtain the current table via
    /// [`Self::actions`] which clones the strong reference under the slot
    /// lock for the duration of one operation.
    actions_slot: SpinNoIrq<Arc<SpinNoIrq<SignalActions>>>,

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
            actions_slot: SpinNoIrq::new(actions),
            default_restorer,
            children: SpinNoIrq::new(Vec::new()),
            possibly_has_signal: AtomicBool::new(false),
        }
    }

    /// Returns a strong reference to the currently-installed signal action
    /// table. The slot lock is held only for the duration of the clone, so
    /// callers can freely lock the returned inner mutex without blocking
    /// concurrent `execve` swap.
    pub fn actions(&self) -> Arc<SpinNoIrq<SignalActions>> {
        self.actions_slot.lock().clone()
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

        // Lock by `actions`. The swappable slot lets `execve` detach the
        // shared inner `Arc<SignalActions>` (with `CLONE_SIGHAND`) without
        // racing this read.
        let actions_arc = self.actions();
        let actions = actions_arc.lock();

        // Check whether the signal is ignored, but only when it is not blocked
        // in all threads AND no thread is waiting for it via sigwaitinfo.
        // POSIX requires that a signal is queued as pending when:
        //   (a) it is blocked in all threads (sigwaitinfo may dequeue it), OR
        //   (b) a thread is specifically waiting for this signal via
        //       rt_sigtimedwait/sigwaitinfo (sigwait_set contains signo).
        // In both cases, applying is_ignore() would silently drop the signal
        // and leave sigwaitinfo sleeping forever.
        let (all_blocked, any_sigwait_for_this) = {
            let children = self.children.lock();
            let all = !children.is_empty()
                && children
                    .iter()
                    .all(|(_, thread)| thread.upgrade().is_none_or(|t| t.signal_blocked(signo)));
            let any = children.iter().any(|(_, thread)| {
                thread
                    .upgrade()
                    .is_some_and(|t| t.sigwait_set.lock().is_some_and(|s| s.has(signo)))
            });
            (all, any)
        };
        if !all_blocked && !any_sigwait_for_this && actions[signo].is_ignore(signo) {
            return None;
        }
        // Drop `actions` before acquiring `self.pending` to maintain a
        // consistent lock ordering (actions → children → pending) and avoid
        // potential deadlocks.
        drop(actions);

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
        *self.actions().lock() = Default::default();
    }

    /// Resets actions across `execve` per POSIX/Linux semantics.
    ///
    /// - Disposition `Handler(_)` → `SIG_DFL` (custom handlers point into
    ///   the old image and must not run in the new one).
    /// - Disposition `Ignore` (explicit `SIG_IGN`) is preserved, with
    ///   flags/mask/restorer cleared — POSIX requires that a parent which
    ///   set `signal(SIGCHLD, SIG_IGN)` keeps that behavior after exec.
    /// - Disposition `Default` is left as `SIG_DFL`; we deliberately do
    ///   *not* upgrade it to explicit `Ignore` even when the signal's
    ///   default action happens to be Ignore (e.g. `SIGCHLD`, `SIGURG`,
    ///   `SIGWINCH`), so a post-exec `sigaction` query observes the
    ///   real disposition the kernel installed.
    ///
    /// The actions slot is **detached** before reset: with `CLONE_SIGHAND`
    /// the inner `Arc<SignalActions>` is shared with one or more peer
    /// processes. Mirror Linux's `unshare_sighand()` — build a fresh
    /// private copy seeded from the current contents and atomically swap
    /// the slot, so the peer's table is left untouched.
    pub fn reset_actions_for_exec(&self) {
        let mut new_actions = {
            let current = self.actions();
            current.lock().clone()
        };
        for signo_idx in 0..64u8 {
            let Some(signo) = Signo::from_repr(signo_idx + 1) else {
                continue;
            };
            let action = &mut new_actions[signo];
            if matches!(action.disposition, crate::SignalDisposition::Ignore) {
                *action = SignalAction {
                    disposition: crate::SignalDisposition::Ignore,
                    ..Default::default()
                };
            } else {
                *action = SignalAction::default();
            }
        }
        *self.actions_slot.lock() = Arc::new(SpinNoIrq::new(new_actions));
    }

    /// Updates a thread's TID in the children registration. Called by
    /// `execve`'s de_thread step so signals targeting the inherited leader
    /// TID resolve to the (renamed) caller thread.
    pub fn rename_child(&self, old_tid: u32, new_tid: u32) {
        let mut children = self.children.lock();
        for entry in children.iter_mut() {
            if entry.0 == old_tid {
                entry.0 = new_tid;
                break;
            }
        }
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
            let actions_arc = self.actions();
            let mut actions = actions_arc.lock();
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
