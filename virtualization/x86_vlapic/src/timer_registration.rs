//! Shared ownership state for host-backed x86 device timers.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_sync::RawSpinLock;

use crate::{
    X86TimerAction, X86TimerCallback, X86VlapicError, X86VlapicResult, host::X86VlapicRuntimeOps,
};

/// Linux KVM's default lower bound for periodic PIT and LAPIC host timers.
///
/// Guest-visible timer state may continue to use the hardware period; this
/// bound controls only how often the host services periodic timer callbacks.
const MIN_PERIODIC_TIMER_PERIOD_NS: u64 = 200_000;

pub(crate) const fn limit_periodic_timer_period_ns(period_ns: u64) -> u64 {
    if period_ns < MIN_PERIODIC_TIMER_PERIOD_NS {
        MIN_PERIODIC_TIMER_PERIOD_NS
    } else {
        period_ns
    }
}

/// Advances one period without replaying a backlog of missed timer edges.
///
/// The absolute target normally advances from its previous value to avoid
/// drift. Linux KVM rearms a late LAPIC timer at `now`, then its pending check
/// coalesces the immediate second callback before advancing another period.
/// This callback already publishes the interrupt, so collapse those two state
/// transitions and restart one period after `now`. Otherwise, a task-context
/// timer callback slower than the guest period can monopolize the host worker.
pub(crate) const fn restart_periodic_deadline_ns(
    deadline_ns: u64,
    interval_ns: u64,
    now_ns: u64,
) -> u64 {
    let next_deadline_ns = deadline_ns.saturating_add(interval_ns);
    if next_deadline_ns <= now_ns {
        now_ns.saturating_add(interval_ns)
    } else {
        next_deadline_ns
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimerArmPhase {
    Armed,
    Firing,
    Retired,
}

struct TimerArmState<T> {
    phase: TimerArmPhase,
    cancel_requested: bool,
    registration_complete: bool,
    handle: Option<T>,
    /// Set with the first cancel request: whether the arm could still deliver
    /// an edge at that moment, so the caller knows a resume is owed once the
    /// arm is quiesced. Persisted on the arm so a retried cancel reports the
    /// same answer.
    resume_owed: bool,
}

struct TimerArm<T> {
    identity: usize,
    state: RawSpinLock<TimerArmState<T>>,
}

impl<T: Copy> TimerArm<T> {
    fn new(identity: usize) -> Self {
        Self {
            identity,
            state: RawSpinLock::new(TimerArmState {
                phase: TimerArmPhase::Armed,
                cancel_requested: false,
                registration_complete: false,
                handle: None,
                resume_owed: false,
            }),
        }
    }

    fn begin_fire(&self) -> bool {
        let mut state = self.state.lock_irqsave();
        if state.cancel_requested || state.phase != TimerArmPhase::Armed {
            return false;
        }
        state.phase = TimerArmPhase::Firing;
        true
    }

    fn finish_fire(&self, requested: X86TimerAction) -> X86TimerAction {
        let mut state = self.state.lock_irqsave();
        assert_eq!(
            state.phase,
            TimerArmPhase::Firing,
            "x86 timer callback must finish one claimed arm"
        );
        if state.cancel_requested {
            // A cancel already claimed this arm; the callback collapses to a
            // single completion. `current` is deliberately kept so a task-side
            // cancel barrier can still observe the host retirement.
            state.phase = TimerArmPhase::Retired;
            return X86TimerAction::Complete;
        }
        match requested {
            X86TimerAction::Complete => {
                state.phase = TimerArmPhase::Retired;
                X86TimerAction::Complete
            }
            X86TimerAction::Rearm(deadline_ns) => {
                state.phase = TimerArmPhase::Armed;
                X86TimerAction::Rearm(deadline_ns)
            }
        }
    }

    /// Publishes the stable host handle for this arm.
    ///
    /// The handle is retained even when the callback already completed, so a
    /// later task-side cancel barrier can observe the host retirement instead
    /// of mistaking a finished callback for quiescence.
    fn finish_registration(&self, handle: T) {
        let mut state = self.state.lock_irqsave();
        assert!(
            !state.registration_complete,
            "x86 timer host registration completed twice"
        );
        state.registration_complete = true;
        state.handle = Some(handle);
    }

    fn fail_registration(&self) {
        let mut state = self.state.lock_irqsave();
        state.registration_complete = true;
        state.phase = TimerArmPhase::Retired;
    }

    /// Requests cancellation and returns the stable handle once the callback
    /// that claimed the arm has retired, together with whether a resume is owed.
    ///
    /// Waiting uses the runtime's task-yield capability, never a spin: the
    /// callback may have been preempted on this same CPU. All waiting happens
    /// outside the raw arm state guard.
    fn request_cancel_and_take_handle(&self, mut wait_progress: impl FnMut()) -> Option<(T, bool)> {
        loop {
            {
                let mut state = self.state.lock_irqsave();
                if !state.cancel_requested {
                    state.resume_owed = state.phase != TimerArmPhase::Retired;
                }
                state.cancel_requested = true;
                // Retain the logical arm until the host confirms retirement.
                // A failed cancellation must preserve the countdown for retry;
                // cancel_requested already prevents a new callback claim.
                if state.registration_complete && state.phase != TimerArmPhase::Firing {
                    return state
                        .handle
                        .take()
                        .map(|handle| (handle, state.resume_owed));
                }
            }
            // Mirrors hrtimer_cancel(): once a callback has claimed the arm,
            // cancellation does not return until that callback has retired.
            // Yield so a preempted callback on this CPU can make progress.
            wait_progress();
        }
    }

    /// Whether the arm can still deliver an edge (armed or firing).
    fn is_active(&self) -> bool {
        self.state.lock_irqsave().phase != TimerArmPhase::Retired
    }

    fn restore_cancel_handle(&self, handle: T) {
        let mut state = self.state.lock_irqsave();
        assert!(state.handle.replace(handle).is_none());
    }
}

/// Owns the single host registration for one x86 device timer.
///
/// APIC and PIT callbacks both enter this state machine before performing a
/// device-visible side effect. Reprogramming first cancels the current arm and
/// waits for a callback that already claimed it, matching Linux
/// `hrtimer_cancel()` ordering. The arm identity is the only stale-callback
/// authority; there is no parallel generation or polling owner.
pub(crate) struct TimerRegistration<R: X86VlapicRuntimeOps> {
    next_arm_identity: AtomicUsize,
    current: RawSpinLock<Option<Arc<TimerArm<R::TimerHandle>>>>,
}

impl<R: X86VlapicRuntimeOps> TimerRegistration<R> {
    pub(crate) const fn new() -> Self {
        Self {
            next_arm_identity: AtomicUsize::new(0),
            current: RawSpinLock::new(None),
        }
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        runtime: &R,
        deadline_ns: u64,
        callback: X86TimerCallback,
    ) -> X86VlapicResult {
        self.register_with(
            runtime,
            deadline_ns,
            callback,
            |runtime, deadline_ns, callback| runtime.register_timer(deadline_ns, callback),
        )
    }

    /// Registers a callback through the host hard-timer capability.
    ///
    /// This is called from task context; only the callback itself runs in hard
    /// IRQ context, so the registration barrier above may yield.
    ///
    /// # Safety
    ///
    /// `callback` and the registration state transitions executed around it
    /// must remain bounded and valid in hard IRQ context.
    pub(crate) unsafe fn register_hard(
        self: &Arc<Self>,
        runtime: &R,
        deadline_ns: u64,
        callback: X86TimerCallback,
    ) -> X86VlapicResult {
        self.register_with(
            runtime,
            deadline_ns,
            callback,
            |runtime, deadline_ns, callback| unsafe {
                runtime.register_hard_timer(deadline_ns, callback)
            },
        )
    }

    fn register_with(
        self: &Arc<Self>,
        runtime: &R,
        deadline_ns: u64,
        mut callback: X86TimerCallback,
        register: impl FnOnce(&R, u64, X86TimerCallback) -> X86VlapicResult<R::TimerHandle>,
    ) -> X86VlapicResult {
        // A previous arm may have run its callback to completion without being
        // cancelled yet; its stable handle is retained for exactly this
        // barrier. Retire it through the full host cancel barrier before
        // arming a newer timer so a retired callback can never race a new guest
        // reprogram, and so a stale handle is never silently dropped.
        self.invalidate_and_cancel(runtime)?;

        let arm = self.begin_arm()?;
        let callback_arm = Arc::clone(&arm);
        let handle = match register(
            runtime,
            deadline_ns,
            alloc::boxed::Box::new(move |now_ns| {
                if !callback_arm.begin_fire() {
                    return X86TimerAction::Complete;
                }
                // The callback never retires its own registration: the stable
                // host handle must stay reachable for the task-side cancel
                // barrier. `finish_fire` only retires the *arm phase*.
                callback_arm.finish_fire(callback(now_ns))
            }),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                arm.fail_registration();
                self.retire(&arm);
                return Err(error);
            }
        };

        arm.finish_registration(handle);
        Ok(())
    }

    /// Whether a host registration is still owned by this object.
    ///
    /// This stays `true` after a one-shot callback completed: the stable handle
    /// is retained until the task side cancels and observes the host
    /// retirement. Callers that must guarantee the producer is quiet (suspend,
    /// stop, reprogram) therefore key on this instead of on the arm phase.
    pub(crate) fn has_registration(&self) -> bool {
        self.current.lock_irqsave().is_some()
    }

    /// Whether the current arm can still deliver an edge (armed or firing).
    ///
    /// A completed one-shot callback leaves a retained handle but no live arm,
    /// so guest-visible "timer running" state keys on this instead of on the
    /// handle's presence.
    pub(crate) fn is_active(&self) -> bool {
        self.current
            .lock_irqsave()
            .as_ref()
            .is_some_and(|arm| arm.is_active())
    }

    /// Quiesces the current host registration, if any, behind the full cancel
    /// barrier and retires it.
    ///
    /// The barrier waits, outside every raw guard, for a callback that already
    /// claimed the arm and for the host to reclaim its payload. Returns whether
    /// a resume is owed: `true` when the arm could still deliver an edge when
    /// this cancellation began. On a host cancellation failure the retained
    /// handle and arm are restored so the caller can retry.
    pub(crate) fn invalidate_and_cancel(&self, runtime: &R) -> X86VlapicResult<bool> {
        let Some(arm) = self.current.lock_irqsave().as_ref().cloned() else {
            return Ok(false);
        };
        let Some((handle, resume_owed)) =
            arm.request_cancel_and_take_handle(|| runtime.wait_timer_progress())
        else {
            // A prior cancel already took the handle; nothing is left to retire.
            self.retire(&arm);
            return Ok(false);
        };
        if let Err(error) = runtime.cancel_timer(handle) {
            arm.restore_cancel_handle(handle);
            self.restore(&arm);
            return Err(error);
        }
        self.retire(&arm);
        Ok(resume_owed)
    }

    fn begin_arm(&self) -> X86VlapicResult<Arc<TimerArm<R::TimerHandle>>> {
        let identity = self
            .next_arm_identity
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |identity| {
                identity.checked_add(1)
            })
            .map_err(|_| X86VlapicError::BadState)?
            .checked_add(1)
            .ok_or(X86VlapicError::BadState)?;
        let arm = Arc::new(TimerArm::new(identity));
        let mut current = self.current.lock_irqsave();
        if current.is_some() {
            return Err(X86VlapicError::BadState);
        }
        *current = Some(Arc::clone(&arm));
        Ok(arm)
    }

    fn retire(&self, arm: &TimerArm<R::TimerHandle>) {
        // Take the shared `Arc` out under the guard but drop it after the guard
        // is released: the callback may still hold the last reference, and
        // dropping it inside the short IRQ-safe critical section could run
        // task-context teardown with interrupts disabled.
        let retired = {
            let mut current = self.current.lock_irqsave();
            if current
                .as_ref()
                .is_some_and(|candidate| candidate.identity == arm.identity)
            {
                current.take()
            } else {
                None
            }
        };
        drop(retired);
    }

    fn restore(&self, arm: &Arc<TimerArm<R::TimerHandle>>) {
        let mut current = self.current.lock_irqsave();
        if current.is_none() {
            *current = Some(Arc::clone(arm));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{limit_periodic_timer_period_ns, restart_periodic_deadline_ns};

    #[test]
    fn host_periodic_timers_share_the_linux_kvm_minimum_period() {
        assert_eq!(limit_periodic_timer_period_ns(1_000), 200_000);
        assert_eq!(limit_periodic_timer_period_ns(250_000), 250_000);
    }

    #[test]
    fn periodic_rearm_advances_from_the_previous_target() {
        assert_eq!(restart_periodic_deadline_ns(100, 10, 105), 110);
    }

    #[test]
    fn late_periodic_rearm_starts_one_period_after_the_published_edge() {
        assert_eq!(restart_periodic_deadline_ns(100, 10, 125), 135);
    }
}
