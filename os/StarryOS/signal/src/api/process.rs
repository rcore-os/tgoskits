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

use ax_runtime::sync::SpinLock;
use linux_raw_sys::general::kernel_sigaction;
use starry_vm::{VmIo, VmPtr, vm_write_slice};

use crate::{
    PendingSignals, SignalAction, SignalInfo, SignalResult, SignalSet, Signo,
    api::ThreadSignalManager,
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
    pending: SpinLock<PendingSignals>,

    /// The signal actions. Held in a swappable slot because `CLONE_SIGHAND`
    /// hands the inner `Arc` to a peer process; `execve` must be able to
    /// detach this manager from that shared inner table (to reset handlers
    /// for the new image) without mutating the table the peer still uses.
    /// Outside of exec, callers should obtain the current table via
    /// [`Self::actions`] which clones the strong reference under the slot
    /// lock for the duration of one operation.
    actions_slot: SpinLock<Arc<SpinLock<SignalActions>>>,

    /// The default restorer function.
    pub(crate) default_restorer: usize,

    /// Thread-level signal managers.
    pub(crate) children: SpinLock<Vec<(u32, Weak<ThreadSignalManager>)>>,

    pub(crate) possibly_has_signal: AtomicBool,
}

impl ProcessSignalManager {
    /// Creates a new process signal manager.
    pub fn new(actions: Arc<SpinLock<SignalActions>>, default_restorer: usize) -> Self {
        Self {
            pending: SpinLock::new(PendingSignals::default()),
            actions_slot: SpinLock::new(actions),
            default_restorer,
            children: SpinLock::new(Vec::new()),
            possibly_has_signal: AtomicBool::new(false),
        }
    }

    /// Returns a strong reference to the currently-installed signal action
    /// table. The slot lock is held only for the duration of the clone, so
    /// callers can freely lock the returned inner mutex without blocking
    /// concurrent `execve` swap.
    pub fn actions(&self) -> Arc<SpinLock<SignalActions>> {
        self.actions_slot.lock_irqsave().clone()
    }

    pub(crate) fn register_child(&self, tid: u32, child: Weak<ThreadSignalManager>) {
        let mut replacement = Vec::new();
        loop {
            let required = self.children.lock().len().saturating_add(1);
            reserve_empty_child_slots(&mut replacement, required);

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

    fn children_snapshot(&self) -> Vec<(u32, Arc<ThreadSignalManager>)> {
        let mut snapshot = Vec::new();
        loop {
            let child_count = self.children.lock().len();
            reserve_empty_child_slots(&mut snapshot, child_count);
            let children = self.children.lock();
            if snapshot.capacity() < children.len() {
                drop(children);
                continue;
            }
            snapshot.extend(children.iter().cloned());
            break;
        }
        // Retain live targets before taking the action lock. Upgrading a Weak
        // temporarily and dropping its last Arc while inspecting dispositions
        // could otherwise run the target destructor inside that critical section.
        let mut live = Vec::with_capacity(snapshot.len());
        for (tid, weak) in snapshot {
            if let Some(thread) = weak.upgrade() {
                live.push((tid, thread));
            } else {
                self.remove_dead_child(tid, &weak);
            }
        }
        live
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
        let mut guard = self.pending.lock_irqsave();
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
        let mut guard = self.pending.lock_irqsave();
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
        // Declare the retained targets first so early returns release the action
        // guard before either the target allocations or the snapshot buffer.
        let children = self.children_snapshot();

        // Lock by `actions`. The swappable slot lets `execve` detach the
        // shared inner `Arc<SignalActions>` (with `CLONE_SIGHAND`) without
        // racing this read.
        let actions_arc = self.actions();
        let actions = actions_arc.lock_irqsave();

        // Check whether the signal is ignored, but only when it is not blocked
        // in all threads AND no thread is waiting for it via sigwaitinfo.
        // POSIX requires that a signal is queued as pending when:
        //   (a) it is blocked in all threads (sigwaitinfo may dequeue it), OR
        //   (b) a thread is specifically waiting for this signal via
        //       rt_sigtimedwait/sigwaitinfo (its sigwait state contains signo).
        // In both cases, applying is_ignore() would silently drop the signal
        // and leave sigwaitinfo sleeping forever.
        let all_blocked = !children.is_empty()
            && children
                .iter()
                .all(|(_, thread)| thread.signal_blocked(signo));
        let any_sigwait_for_this = children
            .iter()
            .any(|(_, thread)| thread.is_sigwait_for(signo));
        if !all_blocked && !any_sigwait_for_this && actions[signo].is_ignore(signo) {
            return None;
        }
        // Pending publication and wake callbacks do not hold the action lock.
        drop(actions);

        if self.pending.lock_irqsave().put_signal(sig) {
            self.possibly_has_signal.store(true, Ordering::Release);
        }
        let result = children
            .iter()
            .find(|(_, thread)| !thread.signal_blocked(signo))
            .map(|(tid, _)| *tid);
        if result.is_none() {
            // The future waker is an arbitrary task-context callback. Invoke it
            // only after dropping the process child registry lock.
            for (_, thread) in &children {
                thread.wake_sigwait(signo);
            }
        }
        result
    }

    /// Gets currently pending signals.
    pub fn pending(&self) -> SignalSet {
        self.pending.lock_irqsave().set
    }

    /// Resets actions to empty.
    pub fn reset_actions(&self) {
        *self.actions().lock_irqsave() = Default::default();
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
            current.lock_irqsave().clone()
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
        let replacement = Arc::new(SpinLock::new(new_actions));
        let previous = core::mem::replace(&mut *self.actions_slot.lock_irqsave(), replacement);
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
    pub fn set_action<I: VmIo>(
        &self,
        vm: &mut I,
        signo: Signo,
        act: *const kernel_sigaction,
        oldact: *mut kernel_sigaction,
    ) -> SignalResult<isize> {
        let new_action = if let Some(act) = act.nullable() {
            let act = unsafe { act.vm_read_uninit(vm)?.assume_init() }.into();
            debug!("sys_rt_sigaction <= signo: {signo:?}, act: {act:?}");
            Some(act)
        } else {
            None
        };

        let old_action = {
            let actions_arc = self.actions();
            let mut actions = actions_arc.lock_irqsave();
            let old = actions[signo].clone();
            if let Some(act) = new_action {
                actions[signo] = act;
            }
            old
        };

        if let Some(oldact) = oldact.nullable() {
            write_kernel_sigaction(vm, oldact, old_action)?;
        }
        Ok(0)
    }
}

fn write_kernel_sigaction<I: VmIo>(
    vm: &mut I,
    oldact: *mut kernel_sigaction,
    action: SignalAction,
) -> SignalResult<()> {
    let action: kernel_sigaction = action.into();
    vm_write_slice(vm, oldact.cast::<usize>(), &kernel_sigaction_words(action))?;
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

fn reserve_empty_child_slots(slots: &mut Vec<(u32, Weak<ThreadSignalManager>)>, required: usize) {
    debug_assert!(slots.is_empty());
    if slots.capacity() < required {
        // reserve_exact counts additional elements from len, which is zero
        // throughout a retry, rather than from the existing capacity.
        slots.reserve_exact(required);
    }
}

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
        let actions = Arc::new(SpinLock::new(SignalActions::default()));
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

    #[test]
    fn process_signal_prepares_and_releases_targets_outside_action_lock() {
        let actions = Arc::new(SpinLock::new(SignalActions::default()));
        let process = Arc::new(ProcessSignalManager::new(Arc::clone(&actions), 0));
        let thread = ThreadSignalManager::new(1, Arc::clone(&process));
        let retired = ThreadSignalManager::new(2, Arc::clone(&process));
        drop(retired);

        let ((ignored, selected), locked_heap_operations) =
            crate::allocation_audit::with_action_lock(&actions, || {
                (
                    process.send_signal(SignalInfo::new_kernel(Signo::SIGCHLD)),
                    process.send_signal(SignalInfo::new_kernel(Signo::SIGUSR1)),
                )
            });

        assert_eq!(ignored, None);
        assert_eq!(selected, Some(1));
        assert!(thread.pending().has(Signo::SIGUSR1));
        assert_eq!(
            locked_heap_operations, 0,
            "signal target allocation or final release held the action lock"
        );
    }

    #[test]
    fn registry_growth_retry_reserves_the_complete_slot_count() {
        let mut slots = Vec::with_capacity(2);
        let required = slots.capacity() + 1;
        reserve_empty_child_slots(&mut slots, required);
        assert!(
            slots.capacity() >= required,
            "registry growth retry made no progress"
        );
        assert!(slots.is_empty());
    }
}
