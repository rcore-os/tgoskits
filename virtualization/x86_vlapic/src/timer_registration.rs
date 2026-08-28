//! Shared ownership state for host-backed x86 device timers.

use alloc::sync::Arc;
use core::{
    hint::spin_loop,
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    X86TimerAction, X86TimerCallback, X86VlapicError, X86VlapicResult,
    host::{self, X86VlapicHostOps},
    lock::SpinMutex,
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
}

struct TimerArm<T> {
    identity: usize,
    state: SpinMutex<TimerArmState<T>>,
}

impl<T: Copy> TimerArm<T> {
    fn new(identity: usize) -> Self {
        Self {
            identity,
            state: SpinMutex::new(TimerArmState {
                phase: TimerArmPhase::Armed,
                cancel_requested: false,
                registration_complete: false,
                handle: None,
            }),
        }
    }

    fn begin_fire(&self) -> bool {
        let mut state = self.state.lock();
        if state.cancel_requested || state.phase != TimerArmPhase::Armed {
            return false;
        }
        state.phase = TimerArmPhase::Firing;
        true
    }

    fn finish_fire(&self, requested: X86TimerAction) -> TimerFireCompletion {
        let mut state = self.state.lock();
        assert_eq!(
            state.phase,
            TimerArmPhase::Firing,
            "x86 timer callback must finish one claimed arm"
        );
        if state.cancel_requested {
            state.phase = TimerArmPhase::Retired;
            return TimerFireCompletion {
                action: X86TimerAction::Complete,
                retire_registration: false,
            };
        }
        match requested {
            X86TimerAction::Complete => {
                state.phase = TimerArmPhase::Retired;
                TimerFireCompletion {
                    action: X86TimerAction::Complete,
                    retire_registration: true,
                }
            }
            X86TimerAction::Rearm(deadline_ns) => {
                state.phase = TimerArmPhase::Armed;
                TimerFireCompletion {
                    action: X86TimerAction::Rearm(deadline_ns),
                    retire_registration: false,
                }
            }
        }
    }

    fn finish_registration(&self, handle: T) -> TimerRegistrationCompletion {
        let mut state = self.state.lock();
        assert!(
            !state.registration_complete,
            "x86 timer host registration completed twice"
        );
        state.registration_complete = true;
        if state.cancel_requested {
            state.handle = Some(handle);
            TimerRegistrationCompletion::CancellationOwnsHandle
        } else if state.phase == TimerArmPhase::Retired {
            TimerRegistrationCompletion::CallbackCompleted
        } else {
            state.handle = Some(handle);
            TimerRegistrationCompletion::Active
        }
    }

    fn fail_registration(&self) {
        let mut state = self.state.lock();
        state.registration_complete = true;
        state.phase = TimerArmPhase::Retired;
    }

    fn request_cancel_and_take_handle(&self) -> Option<T> {
        loop {
            {
                let mut state = self.state.lock();
                state.cancel_requested = true;
                if state.phase == TimerArmPhase::Armed {
                    state.phase = TimerArmPhase::Retired;
                }
                if state.registration_complete && state.phase != TimerArmPhase::Firing {
                    return state.handle.take();
                }
            }
            // Mirrors hrtimer_cancel(): once a callback has claimed the arm,
            // cancellation does not return until that callback has retired.
            // No timer/device lock is held while waiting.
            spin_loop();
        }
    }

    fn restore_cancel_handle(&self, handle: T) {
        let mut state = self.state.lock();
        assert!(state.handle.replace(handle).is_none());
    }
}

struct TimerFireCompletion {
    action: X86TimerAction,
    retire_registration: bool,
}

enum TimerRegistrationCompletion {
    Active,
    CallbackCompleted,
    CancellationOwnsHandle,
}

/// Owns the single host registration for one x86 device timer.
///
/// APIC and PIT callbacks both enter this state machine before performing a
/// device-visible side effect. Reprogramming first cancels the current arm and
/// waits for a callback that already claimed it, matching Linux
/// `hrtimer_cancel()` ordering. The arm identity is the only stale-callback
/// authority; there is no parallel generation or polling owner.
pub(crate) struct TimerRegistration<H: X86VlapicHostOps> {
    next_arm_identity: AtomicUsize,
    current: SpinMutex<Option<Arc<TimerArm<H::TimerHandle>>>>,
    _host: PhantomData<fn() -> H>,
}

impl<H: X86VlapicHostOps> TimerRegistration<H> {
    pub(crate) const fn new() -> Self {
        Self {
            next_arm_identity: AtomicUsize::new(0),
            current: SpinMutex::new(None),
            _host: PhantomData,
        }
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        deadline_ns: u64,
        mut callback: X86TimerCallback,
    ) -> X86VlapicResult {
        let arm = self.begin_arm()?;
        let callback_arm = Arc::clone(&arm);
        let callback_registration = Arc::clone(self);
        let handle = match host::register_timer::<H>(
            deadline_ns,
            alloc::boxed::Box::new(move |now_ns| {
                if !callback_arm.begin_fire() {
                    return X86TimerAction::Complete;
                }
                let completion = callback_arm.finish_fire(callback(now_ns));
                if completion.retire_registration {
                    callback_registration.retire(&callback_arm);
                }
                completion.action
            }),
        ) {
            Ok(handle) => handle,
            Err(error) => {
                arm.fail_registration();
                self.retire(&arm);
                return Err(error);
            }
        };

        match arm.finish_registration(handle) {
            TimerRegistrationCompletion::Active
            | TimerRegistrationCompletion::CancellationOwnsHandle => Ok(()),
            TimerRegistrationCompletion::CallbackCompleted => {
                self.retire(&arm);
                Ok(())
            }
        }
    }

    pub(crate) fn is_armed(&self) -> bool {
        self.current.lock().is_some()
    }

    pub(crate) fn invalidate_and_cancel(&self) -> X86VlapicResult {
        let Some(arm) = self.current.lock().as_ref().cloned() else {
            return Ok(());
        };
        let handle = arm.request_cancel_and_take_handle();
        if let Some(handle) = handle
            && let Err(error) = host::cancel_timer::<H>(handle)
        {
            arm.restore_cancel_handle(handle);
            self.restore(&arm);
            return Err(error);
        }
        self.retire(&arm);
        Ok(())
    }

    fn begin_arm(&self) -> X86VlapicResult<Arc<TimerArm<H::TimerHandle>>> {
        let identity = self
            .next_arm_identity
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |identity| {
                identity.checked_add(1)
            })
            .map_err(|_| X86VlapicError::BadState)?
            .checked_add(1)
            .ok_or(X86VlapicError::BadState)?;
        let arm = Arc::new(TimerArm::new(identity));
        let mut current = self.current.lock();
        if current.is_some() {
            return Err(X86VlapicError::BadState);
        }
        *current = Some(Arc::clone(&arm));
        Ok(arm)
    }

    fn retire(&self, arm: &TimerArm<H::TimerHandle>) {
        let mut current = self.current.lock();
        if current
            .as_ref()
            .is_some_and(|candidate| candidate.identity == arm.identity)
        {
            current.take();
        }
    }

    fn restore(&self, arm: &Arc<TimerArm<H::TimerHandle>>) {
        let mut current = self.current.lock();
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
