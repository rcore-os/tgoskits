//! Runtime ownership of the local physical clockevent.
//!
//! The state transition, bounded scheduler service, and hardware commit stay
//! inside one local-IRQ exclusion window. The physical timer has no remote
//! mutable endpoint; remote producers publish scheduler deadlines through
//! ax-task and let the owning CPU reconcile them here.
pub(crate) fn monotonic_now() -> ax_task::runtime::MonotonicInstant {
    ax_task::runtime::MonotonicInstant::from_nanos(ax_hal::time::monotonic_time_nanos())
        .expect("platform monotonic clock exceeded the signed ktime domain")
}
fn periodic_interval_nanos() -> u64 {
    // This is the periodic wakeup source for an active CPU, not a task-switch
    // latency bound. Linux-style NOHZ idle explicitly stops this clockevent.
    let interval = crate::build_info::SCHEDULER_TICK_INTERVAL_NANOS;
    assert_ne!(interval, 0, "scheduler tick interval must be non-zero");
    interval
}
#[ax_percpu::def_percpu]
static LOCAL_CLOCK_EVENT: crate::clock_event::LocalClockEvent =
    crate::clock_event::LocalClockEvent::offline();
fn with_local_clock_event_mut<R>(
    operation: impl for<'value> FnOnce(&'value mut crate::clock_event::LocalClockEvent) -> R,
) -> R {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "mutable clockevent access requires local IRQ exclusion"
    );
    // SAFETY: every caller is either offline initialization or the local timer
    // IRQ/scheduler path with IRQs disabled. The clockevent has no remote
    // mutable endpoint, so this excludes every conflicting access.
    unsafe { ax_percpu::with_cpu_pin(|pin| with_local_clock_event_mut_pinned(pin, operation)) }
        .unwrap_or_else(|error| panic!("clockevent CPU-local state is invalid: {error}"))
}

fn with_local_clock_event_mut_pinned<R>(
    pin: &cpu_local::CpuPin<'_>,
    operation: impl for<'value> FnOnce(&'value mut crate::clock_event::LocalClockEvent) -> R,
) -> R {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "mutable clockevent access requires local IRQ exclusion"
    );
    // SAFETY: the caller's local IRQ exclusion covers the supplied pin and
    // prevents every conflicting clockevent owner access.
    unsafe {
        ax_percpu::with_exclusive_cpu(pin, |exclusive| {
            LOCAL_CLOCK_EVENT.with_current_mut(exclusive, operation)
        })
    }
}
fn apply_clock_event_action(action: crate::clock_event::ClockEventAction) {
    match action {
        crate::clock_event::ClockEventAction::None => {}
        crate::clock_event::ClockEventAction::Stop => ax_hal::time::cancel_oneshot_timer(),
        crate::clock_event::ClockEventAction::Resume(deadline) => {
            ax_hal::time::resume_oneshot_timer(deadline.as_nanos());
        }
        crate::clock_event::ClockEventAction::Program(deadline) => {
            ax_hal::time::set_oneshot_timer(deadline.as_nanos());
        }
    }
}
pub(crate) fn take_current_clock_event_offline() {
    run_clock_event_transaction(
        crate::sync::IrqSaveGuard::new,
        || {
            (
                (),
                with_local_clock_event_mut(crate::clock_event::LocalClockEvent::take_offline),
            )
        },
        apply_clock_event_action,
    );
}
fn run_clock_event_transaction<R, Action, Guard>(
    acquire_irq: impl FnOnce() -> Guard,
    access: impl FnOnce() -> (R, Action),
    apply: impl FnOnce(Action),
) -> R {
    run_clock_event_irq_scope(acquire_irq, || {
        let (result, action) = access();
        apply(action);
        result
    })
}
fn run_clock_event_irq_scope<R, Guard>(
    acquire_irq: impl FnOnce() -> Guard,
    service: impl FnOnce() -> R,
) -> R {
    // One IRQ-save must cover clockevent state transitions, bounded task work,
    // and the physical commit so an IRQ cannot observe a split transaction.
    let irq_guard = acquire_irq();
    let result = service();
    drop(irq_guard);
    result
}
fn commit_local_clock_event<R>(
    operation: impl for<'value> FnOnce(
        &'value mut crate::clock_event::LocalClockEvent,
    ) -> (R, crate::clock_event::ClockEventAction),
) -> R {
    run_clock_event_transaction(
        crate::sync::IrqSaveGuard::new,
        || with_local_clock_event_mut(operation),
        apply_clock_event_action,
    )
}
pub(crate) fn enable_irqs_after_scheduler_online(_online: crate::task::PublishedCpuOnline) {
    ax_hal::asm::enable_irqs();
}
#[must_use = "a claimed clockevent firing transaction must be finished"]
struct ClockEventFiringTransaction {
    token: crate::clock_event::ClockEventFiringToken,
    periodic_tick: bool,
}
impl ClockEventFiringTransaction {
    fn begin(
        now: ax_task::runtime::MonotonicInstant,
    ) -> Result<Self, crate::clock_event::ClockEventAction> {
        let claim = with_local_clock_event_mut(|clockevent| clockevent.claim_irq(now));
        let token = match claim {
            // No logical owner may leave a level/pending clockevent source
            // armed across EOI. This is the clockevent-device shutdown step,
            // independent of the interrupt controller acknowledgement.
            crate::clock_event::ClockEventIrqClaim::Ignored => {
                return Err(crate::clock_event::ClockEventAction::Stop);
            }
            crate::clock_event::ClockEventIrqClaim::Firing(token) => token,
        };
        // Linux clockevent drivers quiesce a claimed source before invoking
        // the hrtimer callback. In particular, a level-triggered architectural
        // timer must be masked before interrupt-controller EOI can repend it.
        apply_clock_event_action(token.quiesce_action());
        let periodic_tick = with_local_clock_event_mut(|clockevent| {
            clockevent.advance_periodic(now, periodic_interval_nanos())
        });
        Ok(Self {
            token,
            periodic_tick,
        })
    }

    fn finish(self, outcome: ax_task::TaskClockEventOutcome) {
        let token = self.token;
        let action = with_local_clock_event_mut(|clockevent| {
            let _ = clockevent.publish_scheduler(
                outcome.update().generation(),
                outcome.update().deadline(),
                outcome.update().runtime_deadline(),
            );
            let rearm = crate::clock_event::ClockEventRearm::Deferred;
            clockevent.finish_firing(token, rearm)
        });
        apply_clock_event_action(action);
    }
    fn finish_early(self) {
        let action = with_local_clock_event_mut(|clockevent| {
            clockevent.finish_firing(self.token, crate::clock_event::ClockEventRearm::Deferred)
        });
        apply_clock_event_action(action);
    }
    const fn periodic_tick(&self) -> bool {
        self.periodic_tick
    }
    const fn scheduler_deadline_elapsed(&self) -> bool {
        self.token.scheduler_deadline_elapsed()
    }
    const fn logical_deadline_elapsed(&self) -> bool {
        self.token.logical_deadline_elapsed()
    }
}

const fn scheduler_service_required(
    periodic_tick_elapsed: bool,
    logical_deadline_elapsed: bool,
) -> bool {
    periodic_tick_elapsed || logical_deadline_elapsed
}
pub(crate) fn local_clock_event_has_immediate_work(
    now: ax_task::runtime::MonotonicInstant,
) -> bool {
    commit_local_clock_event(|clockevent| {
        (
            clockevent.has_immediate_work(now),
            crate::clock_event::ClockEventAction::None,
        )
    })
}
pub(crate) fn stop_current_scheduler_tick_for_idle() {
    commit_local_clock_event(|clockevent| ((), clockevent.stop_scheduler_tick_for_idle()));
}
pub(crate) fn restart_current_scheduler_tick_after_idle(now: ax_task::runtime::MonotonicInstant) {
    commit_local_clock_event(|clockevent| {
        (
            (),
            clockevent.restart_scheduler_tick_after_idle(now, periodic_interval_nanos()),
        )
    });
}
pub(crate) fn publish_local_scheduler_deadline(update: ax_task::runtime::SchedulerDeadlineUpdate) {
    commit_local_clock_event(|clockevent| {
        (
            (),
            clockevent.publish_scheduler(
                update.generation(),
                update.deadline(),
                update.runtime_deadline(),
            ),
        )
    });
}

/// Completes Linux-style deferred hrtimer rearm before local IRQ restoration.
pub(crate) fn finish_deferred_rearm() {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "deferred clockevent rearm requires local IRQ exclusion"
    );
    let action = with_local_clock_event_mut(|clockevent| clockevent.finish_deferred_rearm());
    apply_clock_event_action(action);
}

pub(crate) fn finish_deferred_rearm_pinned(pin: &cpu_local::CpuPin<'_>) {
    assert!(
        !ax_hal::asm::irqs_enabled(),
        "deferred clockevent rearm requires local IRQ exclusion"
    );
    let action =
        with_local_clock_event_mut_pinned(pin, |clockevent| clockevent.finish_deferred_rearm());
    apply_clock_event_action(action);
}
pub(crate) fn init_timer() {
    run_clock_event_transaction(
        crate::sync::IrqSaveGuard::new,
        || {
            let now = monotonic_now();
            let periodic = initial_periodic_deadline(now, periodic_interval_nanos());
            let action = with_local_clock_event_mut(|clockevent| clockevent.online(periodic));
            ((), action)
        },
        apply_clock_event_action,
    );
}
pub(crate) fn initial_periodic_deadline(
    now: ax_task::runtime::MonotonicInstant,
    interval_ns: u64,
) -> crate::clock_event::ClockDeadline {
    assert_ne!(
        interval_ns, 0,
        "periodic clockevent interval must be non-zero"
    );
    let deadline = now.deadline_after(core::time::Duration::from_nanos(interval_ns));
    assert!(
        !now.reached(deadline),
        "periodic scheduler tick exceeded the finite monotonic clock domain"
    );
    crate::clock_event::ClockDeadline::from_monotonic(deadline)
}
pub(crate) fn next_periodic_deadline(
    deadline: crate::clock_event::ClockDeadline,
    now: ax_task::runtime::MonotonicInstant,
    interval_ns: u64,
) -> crate::clock_event::ClockDeadline {
    assert_ne!(
        interval_ns, 0,
        "periodic clockevent interval must be non-zero"
    );
    if !now.reached(deadline.as_monotonic()) {
        return deadline;
    }

    let deadline_ns = deadline.as_nanos();
    let now_ns = now.as_nanos();
    let elapsed_ns = (now_ns - deadline_ns) as u128;
    let interval_ns = interval_ns as u128;
    let periods = elapsed_ns / interval_ns + 1;
    let next = deadline_ns as u128 + periods * interval_ns;
    let next =
        u64::try_from(next).expect("periodic scheduler tick exceeded the physical clock domain");
    let next = crate::clock_event::ClockDeadline::from_nanos(next)
        .expect("periodic scheduler tick exceeded the finite monotonic clock domain");
    assert!(
        !now.reached(next.as_monotonic()),
        "periodic scheduler tick must advance beyond the current instant"
    );
    next
}
pub(crate) fn timer_irq_handler(ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    run_clock_event_irq_scope(crate::sync::IrqSaveGuard::new, || {
        let _ = ctx;
        // Claiming first invalidates the armed device state, matching Linux
        // `hrtimer_interrupt()` setting `expires_next = KTIME_MAX` before it
        // advances scheduler time or runs any hard timer.
        let firing = match ClockEventFiringTransaction::begin(monotonic_now()) {
            Ok(firing) => firing,
            Err(action) => {
                apply_clock_event_action(action);
                return ax_hal::irq::IrqReturn::Handled;
            }
        };
        if !scheduler_service_required(firing.periodic_tick(), firing.logical_deadline_elapsed()) {
            firing.finish_early();
            return ax_hal::irq::IrqReturn::Handled;
        }
        // SAFETY: the claimed local firing transaction excludes migration and
        // nested scheduler-clock publication for this complete stamp.
        unsafe { ax_hal::time::scheduler_clock_tick() }
            .expect("current CPU scheduler clock must be online before timer IRQs");
        let now = monotonic_now();
        let periodic_tick_ns = firing.periodic_tick().then(|| {
            core::num::NonZeroU64::new(periodic_interval_nanos())
                .expect("scheduler tick interval was validated as nonzero")
        });
        let scheduler_event = ax_task::ClaimedSchedulerDeadlines::new(
            periodic_tick_ns,
            firing.scheduler_deadline_elapsed(),
        );
        let outcome = crate::task::on_clock_event(now, scheduler_event);
        if let Some(tick_ns) = periodic_tick_ns {
            crate::task::publish_scheduler_tick(outcome.scheduler_tick_stamp(), tick_ns.get());
        }
        firing.finish(outcome);
        ax_hal::irq::IrqReturn::Handled
    })
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    fn instant(nanos: u64) -> ax_task::runtime::MonotonicInstant {
        ax_task::runtime::MonotonicInstant::from_nanos(nanos).unwrap()
    }

    fn deadline(nanos: u64) -> crate::clock_event::ClockDeadline {
        crate::clock_event::ClockDeadline::from_nanos(nanos).unwrap()
    }
    struct TestIrqGuard<'state> {
        irq_enabled: &'state Cell<bool>,
        restore_enabled: bool,
    }
    impl Drop for TestIrqGuard<'_> {
        fn drop(&mut self) {
            self.irq_enabled.set(self.restore_enabled);
        }
    }
    #[test]
    fn clockevent_transaction_holds_irq_exclusion_through_hardware_commit() {
        let irq_enabled = Cell::new(true);
        let hardware_committed = Cell::new(false);
        let deadline = crate::clock_event::ClockDeadline::from_nanos(100).unwrap();
        let mut clockevent = crate::clock_event::LocalClockEvent::offline();

        let result = super::run_clock_event_transaction(
            || {
                let restore_enabled = irq_enabled.replace(false);
                TestIrqGuard {
                    irq_enabled: &irq_enabled,
                    restore_enabled,
                }
            },
            || {
                assert!(
                    !irq_enabled.get(),
                    "clockevent state mutation requires local IRQ exclusion"
                );
                (7, clockevent.online(deadline))
            },
            |action| {
                assert!(
                    !irq_enabled.get(),
                    "clockevent hardware commit requires the same IRQ exclusion window"
                );
                assert_eq!(
                    action,
                    crate::clock_event::ClockEventAction::Resume(deadline)
                );
                hardware_committed.set(true);
            },
        );

        assert_eq!(result, 7);
        assert_eq!(
            clockevent.phase(),
            crate::clock_event::ClockEventPhase::Armed
        );
        assert_eq!(clockevent.armed_deadline(), Some(deadline));
        assert!(hardware_committed.get());
        assert!(irq_enabled.get(), "the caller's IRQ state must be restored");
    }
    #[test]
    fn timer_irq_scope_establishes_local_irq_exclusion() {
        let irq_enabled = Cell::new(true);

        let handled = super::run_clock_event_irq_scope(
            || {
                let restore_enabled = irq_enabled.replace(false);
                TestIrqGuard {
                    irq_enabled: &irq_enabled,
                    restore_enabled,
                }
            },
            || {
                assert!(
                    !irq_enabled.get(),
                    "timer IRQ service must establish its own local IRQ exclusion"
                );
                true
            },
        );

        assert!(handled);
        assert!(irq_enabled.get(), "the caller's IRQ state must be restored");
    }
    #[test]
    fn stale_edge_without_a_logical_owner_does_not_guess_hardware_state() {
        let mut clockevent = crate::clock_event::LocalClockEvent::offline();
        assert_eq!(
            clockevent.claim_irq(instant(1)),
            crate::clock_event::ClockEventIrqClaim::Ignored
        );
    }
    #[test]
    fn scheduler_tick_interval_honors_build_configuration() {
        let configured_milliseconds = option_env!("AX_SCHEDULER_TICK_MS")
            .unwrap_or("10")
            .parse::<u64>()
            .expect("test scheduler tick interval must be decimal milliseconds");

        assert_eq!(
            super::periodic_interval_nanos(),
            configured_milliseconds * 1_000_000
        );
    }

    #[test]
    fn only_elapsed_logical_deadlines_enter_scheduler_service() {
        assert!(!super::scheduler_service_required(false, false));
        assert!(super::scheduler_service_required(true, false));
        assert!(super::scheduler_service_required(false, true));
        assert!(super::scheduler_service_required(true, true));
    }

    #[test]
    fn periodic_deadline_catches_up_without_accumulating_drift() {
        assert_eq!(
            super::next_periodic_deadline(deadline(100), instant(100), 25),
            deadline(125)
        );
        assert_eq!(
            super::next_periodic_deadline(deadline(100), instant(149), 25),
            deadline(150)
        );
        assert_eq!(
            super::next_periodic_deadline(deadline(100), instant(150), 25),
            deadline(175)
        );
    }

    #[test]
    fn initial_periodic_deadline_saturates_at_the_finite_monotonic_limit() {
        let now = instant(ax_task::runtime::KTIME_MAX_NANOS - 1);
        assert_eq!(
            super::initial_periodic_deadline(now, 2),
            deadline(ax_task::runtime::KTIME_MAX_NANOS)
        );
        assert_eq!(
            super::initial_periodic_deadline(now, 1),
            deadline(ax_task::runtime::KTIME_MAX_NANOS)
        );
    }

    #[test]
    #[should_panic(expected = "finite monotonic clock domain")]
    fn periodic_deadline_overflow_is_a_fatal_clock_domain_violation() {
        let _ = super::next_periodic_deadline(
            deadline(ax_task::runtime::KTIME_MAX_NANOS - 2),
            instant(ax_task::runtime::KTIME_MAX_NANOS - 1),
            1_000_000_000,
        );
    }
}
