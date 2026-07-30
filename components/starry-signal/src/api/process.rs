use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    array,
    mem::{offset_of, size_of},
    ops::{Index, IndexMut},
    sync::atomic::{AtomicBool, Ordering},
};

use ax_errno::AxResult;
use ax_kspin::SpinNoIrq;
use linux_raw_sys::general::kernel_sigaction;
use starry_vm::{VmPtr, vm_write_slice};

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

    pub(crate) fn register_child(&self, tid: u32, child: Weak<ThreadSignalManager>) {
        let mut replacement = Vec::new();
        loop {
            let required = self.children.lock().len().saturating_add(1);
            if replacement.capacity() < required {
                replacement.reserve_exact(required - replacement.capacity());
            }

            let mut children = self.children.lock();
            if replacement.capacity() < children.len().saturating_add(1) {
                drop(children);
                continue;
            }
            replacement.append(&mut children);
            replacement.push((tid, child));
            core::mem::swap(&mut *children, &mut replacement);
            drop(children);
            // The replaced allocation is empty and is released after the
            // IRQ-disabled registry guard has gone away.
            drop(replacement);
            return;
        }
    }

    fn children_snapshot(&self) -> Vec<(u32, Weak<ThreadSignalManager>)> {
        let mut snapshot = Vec::new();
        loop {
            let child_count = self.children.lock().len();
            if snapshot.capacity() < child_count {
                snapshot.reserve_exact(child_count - snapshot.capacity());
            }
            let children = self.children.lock();
            if snapshot.capacity() < children.len() {
                drop(children);
                continue;
            }
            snapshot.extend(children.iter().cloned());
            return snapshot;
        }
    }

    fn remove_dead_child(&self, tid: u32, dead: &Weak<ThreadSignalManager>) {
        let removed = {
            let mut children = self.children.lock();
            children
                .iter()
                .position(|(registered_tid, child)| {
                    *registered_tid == tid && Weak::ptr_eq(child, dead)
                })
                .map(|index| children.swap_remove(index))
        };
        // A final Weak drop may release the allocation. Keep it out of the
        // non-sleeping registry lock.
        drop(removed);
    }

    pub(crate) fn dequeue_signal(&self, mask: &SignalSet) -> Option<SignalInfo> {
        let mut guard = self.pending.lock();
        let result = guard.dequeue_signal(mask);
        if guard.set.is_empty() {
            self.possibly_has_signal.store(false, Ordering::Release);
        }
        result
    }

    /// Dequeues a synchronous (instruction-generated) shared pending signal, if
    /// any. Mirrors [`PendingSignals::dequeue_synchronous_signal`]; used by the
    /// delivery path to give a process-directed fault priority over other
    /// pending signals.
    pub(crate) fn dequeue_synchronous_signal(&self, mask: &SignalSet) -> Option<SignalInfo> {
        let mut guard = self.pending.lock();
        let result = guard.dequeue_synchronous_signal(mask);
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
        //       rt_sigtimedwait/sigwaitinfo (its sigwait state contains signo).
        // In both cases, applying is_ignore() would silently drop the signal
        // and leave sigwaitinfo sleeping forever.
        let children = self.children_snapshot();
        let all_blocked = !children.is_empty()
            && children
                .iter()
                .all(|(_, thread)| thread.upgrade().is_none_or(|t| t.signal_blocked(signo)));
        let any_sigwait_for_this = children.iter().any(|(_, thread)| {
            thread
                .upgrade()
                .is_some_and(|thread| thread.is_sigwait_for(signo))
        });
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
        let mut dead_children = Vec::new();
        let mut waiters = Vec::new();
        for (tid, weak) in &children {
            let Some(thread) = weak.upgrade() else {
                dead_children.push((*tid, weak.clone()));
                continue;
            };
            if result.is_none() && !thread.signal_blocked(signo) {
                result = Some(*tid);
            }
            if thread.is_sigwait_for(signo) {
                waiters.push(thread);
            }
        }
        for (tid, dead) in &dead_children {
            self.remove_dead_child(*tid, dead);
        }
        if result.is_none() {
            // The future waker is an arbitrary task-context callback. Invoke it
            // only after dropping the process child registry lock.
            for thread in waiters {
                thread.wake_sigwait(signo);
            }
        }
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
        let replacement = Arc::new(SpinNoIrq::new(new_actions));
        let previous = core::mem::replace(&mut *self.actions_slot.lock(), replacement);
        // The old Arc may own the final allocation reference.
        drop(previous);
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
            write_kernel_sigaction(oldact, old_action)?;
        }
        Ok(0)
    }
}

fn write_kernel_sigaction(oldact: *mut kernel_sigaction, action: SignalAction) -> AxResult<()> {
    let action: kernel_sigaction = action.into();
    vm_write_slice(oldact.cast::<usize>(), &kernel_sigaction_words(action))?;
    Ok(())
}

#[cfg(sa_restorer)]
fn kernel_sigaction_words(action: kernel_sigaction) -> [usize; 4] {
    [
        action
            .sa_handler_kernel
            .map_or(0, |handler| handler as usize),
        action.sa_flags as usize,
        action.sa_restorer.map_or(0, |restorer| restorer as usize),
        action.sa_mask.sig[0] as usize,
    ]
}

#[cfg(not(sa_restorer))]
fn kernel_sigaction_words(action: kernel_sigaction) -> [usize; 3] {
    [
        action
            .sa_handler_kernel
            .map_or(0, |handler| handler as usize),
        action.sa_flags as usize,
        action.sa_mask.sig[0] as usize,
    ]
}

#[cfg(sa_restorer)]
const _: () = {
    assert!(size_of::<kernel_sigaction>() == 4 * size_of::<usize>());
    assert!(offset_of!(kernel_sigaction, sa_handler_kernel) == 0);
    assert!(offset_of!(kernel_sigaction, sa_flags) == size_of::<usize>());
    assert!(offset_of!(kernel_sigaction, sa_restorer) == 2 * size_of::<usize>());
    assert!(offset_of!(kernel_sigaction, sa_mask) == 3 * size_of::<usize>());
};

#[cfg(not(sa_restorer))]
const _: () = {
    assert!(size_of::<kernel_sigaction>() == 3 * size_of::<usize>());
    assert!(offset_of!(kernel_sigaction, sa_handler_kernel) == 0);
    assert!(offset_of!(kernel_sigaction, sa_flags) == size_of::<usize>());
    assert!(offset_of!(kernel_sigaction, sa_mask) == 2 * size_of::<usize>());
};

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, task::Wake};
    use core::{
        sync::atomic::{AtomicUsize, Ordering},
        task::Waker,
    };

    use super::*;

    struct CountWake(AtomicUsize);

    impl Wake for CountWake {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn blocked_process_signal_wakes_the_matching_sigwait_future() {
        let actions = Arc::new(SpinNoIrq::new(SignalActions::default()));
        let process = Arc::new(ProcessSignalManager::new(actions, 0));
        let mut blocked = SignalSet::default();
        blocked.add(Signo::SIGCHLD);
        let thread = ThreadSignalManager::new_with_blocked(1, Arc::clone(&process), blocked);
        let counter = Arc::new(CountWake(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&counter));

        thread.begin_sigwait(blocked);
        thread.register_sigwait_waker(&waker);

        assert_eq!(
            process.send_signal(SignalInfo::new_kernel(Signo::SIGCHLD)),
            None
        );
        assert_eq!(counter.0.load(Ordering::Relaxed), 1);
    }
}
