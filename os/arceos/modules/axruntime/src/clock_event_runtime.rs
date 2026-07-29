//! Runtime ownership of the local physical clockevent.
//!
//! The state transition, bounded scheduler service, and hardware commit stay
//! inside one local-IRQ exclusion window. The physical timer has no remote
//! mutable endpoint; remote producers publish scheduler deadlines through
//! ax-task and let the owning CPU reconcile them here.

#[cfg(feature = "irq")]
fn ticks_per_sec() -> u64 {
    crate::build_info::TICKS_PER_SEC as u64
}

#[cfg(feature = "irq")]
fn periodic_interval_nanos() -> u64 {
    (ax_hal::time::NANOS_PER_SEC / ticks_per_sec()).max(1)
}

#[cfg(feature = "irq")]
#[ax_percpu::def_percpu]
static LOCAL_CLOCK_EVENT: crate::clock_event::LocalClockEvent =
    crate::clock_event::LocalClockEvent::offline();

#[cfg(feature = "irq")]
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
    unsafe {
        ax_percpu::with_cpu_pin(|pin| {
            ax_percpu::with_exclusive_cpu(pin, |exclusive| {
                LOCAL_CLOCK_EVENT.with_current_mut(exclusive, operation)
            })
        })
    }
    .unwrap_or_else(|error| panic!("clockevent CPU-local state is invalid: {error}"))
}

#[cfg(feature = "irq")]
fn apply_clock_event_action(action: crate::clock_event::ClockEventAction) {
    match action {
        crate::clock_event::ClockEventAction::None => {}
        crate::clock_event::ClockEventAction::Stop => ax_hal::time::cancel_oneshot_timer(),
        crate::clock_event::ClockEventAction::Program(deadline) => {
            ax_hal::time::set_oneshot_timer(deadline.as_nanos());
        }
    }
}

#[cfg(feature = "irq")]
pub(crate) fn take_current_clock_event_offline() {
    run_clock_event_transaction(
        ax_kernel_guard::IrqSave::new,
        || {
            (
                (),
                with_local_clock_event_mut(crate::clock_event::LocalClockEvent::take_offline),
            )
        },
        apply_clock_event_action,
    );
}

#[cfg(feature = "irq")]
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

#[cfg(feature = "irq")]
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

#[cfg(all(feature = "irq", feature = "multitask"))]
fn commit_local_clock_event<R>(
    operation: impl for<'value> FnOnce(
        &'value mut crate::clock_event::LocalClockEvent,
    ) -> (R, crate::clock_event::ClockEventAction),
) -> R {
    run_clock_event_transaction(
        ax_kernel_guard::IrqSave::new,
        || with_local_clock_event_mut(operation),
        apply_clock_event_action,
    )
}

#[cfg(all(feature = "irq", feature = "multitask"))]
pub(crate) fn enable_irqs_after_scheduler_online(_online: crate::task::PublishedCpuOnline) {
    ax_hal::asm::enable_irqs();
}

#[cfg(feature = "irq")]
struct ClockEventFiringGuard {
    active: bool,
}

#[cfg(feature = "irq")]
impl ClockEventFiringGuard {
    fn begin(now_ns: u64) -> Self {
        with_local_clock_event_mut(|clockevent| {
            clockevent.begin_firing();
            clockevent.advance_periodic(now_ns, periodic_interval_nanos());
        });
        Self { active: true }
    }

    #[cfg(feature = "multitask")]
    fn begin_if_due(now_ns: u64) -> Option<Self> {
        let active = with_local_clock_event_mut(|clockevent| {
            if !clockevent.begin_firing_if_due(now_ns) {
                return false;
            }
            clockevent.advance_periodic(now_ns, periodic_interval_nanos());
            true
        });
        active.then_some(Self { active: true })
    }

    fn finish(
        mut self,
        #[cfg(feature = "multitask")] task_update: Option<ax_task::runtime::TaskDeadlineUpdate>,
    ) {
        let action = with_local_clock_event_mut(|clockevent| {
            #[cfg(feature = "multitask")]
            if let Some(update) = task_update {
                let _ = clockevent.publish_task(
                    update.generation(),
                    update
                        .deadline()
                        .map(ax_task::runtime::MonotonicDeadline::as_nanos),
                    update.deferred_work(),
                );
            }
            clockevent.finish_firing()
        });
        self.active = false;
        apply_clock_event_action(action);
    }
}

#[cfg(feature = "irq")]
impl Drop for ClockEventFiringGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let action =
            with_local_clock_event_mut(crate::clock_event::LocalClockEvent::recover_firing);
        apply_clock_event_action(action);
    }
}

#[cfg(all(feature = "irq", feature = "multitask"))]
pub(crate) fn local_clock_event_has_immediate_work(now_ns: u64) -> bool {
    commit_local_clock_event(|clockevent| {
        (
            clockevent.has_immediate_work(now_ns),
            crate::clock_event::ClockEventAction::None,
        )
    })
}

#[cfg(all(feature = "irq", feature = "multitask"))]
pub(crate) fn recover_overdue_local_clock_event(now_ns: u64) -> bool {
    let Some(firing) = ClockEventFiringGuard::begin_if_due(now_ns) else {
        return false;
    };
    let task_update = crate::task::recover_clock_event(now_ns);
    firing.finish(task_update);
    true
}

#[cfg(all(feature = "irq", feature = "multitask"))]
pub(crate) fn publish_local_task_deadline(
    update: ax_task::runtime::TaskDeadlineUpdate,
) -> ax_task::runtime::RuntimeStatus {
    commit_local_clock_event(|clockevent| {
        (
            (),
            clockevent.publish_task(
                update.generation(),
                update
                    .deadline()
                    .map(ax_task::runtime::MonotonicDeadline::as_nanos),
                update.deferred_work(),
            ),
        )
    });
    ax_task::runtime::RuntimeStatus::Success
}

#[cfg(feature = "irq")]
pub(crate) fn init_timer() {
    run_clock_event_transaction(
        ax_kernel_guard::IrqSave::new,
        || {
            let now_ns = ax_hal::time::monotonic_time_nanos();
            let periodic = initial_periodic_deadline(now_ns, periodic_interval_nanos());
            let action = with_local_clock_event_mut(|clockevent| clockevent.online(periodic));
            ((), action)
        },
        apply_clock_event_action,
    );
}

#[cfg(any(feature = "irq", test))]
const fn initial_periodic_deadline(
    now_ns: u64,
    interval_ns: u64,
) -> Option<crate::clock_event::ClockDeadline> {
    match now_ns.checked_add(interval_ns) {
        Some(deadline_ns) => crate::clock_event::ClockDeadline::from_nanos(deadline_ns),
        None => None,
    }
}

#[cfg(any(feature = "irq", test))]
pub(crate) const fn next_periodic_deadline(
    deadline_ns: u64,
    now_ns: u64,
    interval_ns: u64,
) -> Option<u64> {
    if now_ns == u64::MAX {
        return None;
    }
    if deadline_ns > now_ns {
        return Some(deadline_ns);
    }

    let interval_ns = if interval_ns == 0 { 1 } else { interval_ns };
    let elapsed_ns = (now_ns - deadline_ns) as u128;
    let interval_ns = interval_ns as u128;
    let periods = elapsed_ns / interval_ns + 1;
    let next = deadline_ns as u128 + periods * interval_ns;
    if next >= u64::MAX as u128 {
        None
    } else {
        Some(next as u64)
    }
}

#[cfg(any(feature = "multitask", test))]
pub(crate) const fn timer_resolution_from_frequency(frequency_hz: u64) -> u64 {
    if frequency_hz == 0 {
        return ax_hal::time::NANOS_PER_SEC;
    }
    let nanos_per_second = ax_hal::time::NANOS_PER_SEC as u128;
    let frequency_hz = frequency_hz as u128;
    let resolution_ns = nanos_per_second.div_ceil(frequency_hz);
    if resolution_ns == 0 {
        1
    } else {
        resolution_ns as u64
    }
}

#[cfg(feature = "irq")]
pub(crate) fn timer_irq_handler(ctx: ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn {
    run_clock_event_irq_scope(ax_kernel_guard::IrqSave::new, || {
        let _ = ctx;
        let now_ns = ax_hal::time::monotonic_time_nanos();
        let firing = ClockEventFiringGuard::begin(now_ns);
        #[cfg(feature = "multitask")]
        let task_update = crate::task::on_clock_event(now_ns);
        firing.finish(
            #[cfg(feature = "multitask")]
            task_update,
        );
        ax_hal::irq::IrqReturn::Handled
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "irq")]
    use core::cell::Cell;

    #[cfg(feature = "irq")]
    struct TestIrqGuard<'state> {
        irq_enabled: &'state Cell<bool>,
        restore_enabled: bool,
    }

    #[cfg(feature = "irq")]
    impl Drop for TestIrqGuard<'_> {
        fn drop(&mut self) {
            self.irq_enabled.set(self.restore_enabled);
        }
    }

    #[cfg(feature = "irq")]
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
                (7, clockevent.online(Some(deadline)))
            },
            |action| {
                assert!(
                    !irq_enabled.get(),
                    "clockevent hardware commit requires the same IRQ exclusion window"
                );
                assert_eq!(
                    action,
                    crate::clock_event::ClockEventAction::Program(deadline)
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

    #[cfg(feature = "irq")]
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
    fn periodic_deadline_catches_up_without_accumulating_drift() {
        assert_eq!(super::next_periodic_deadline(100, 100, 25), Some(125));
        assert_eq!(super::next_periodic_deadline(100, 149, 25), Some(150));
        assert_eq!(super::next_periodic_deadline(100, 150, 25), Some(175));
    }

    #[test]
    fn initial_periodic_deadline_becomes_idle_at_the_monotonic_limit() {
        assert_eq!(super::initial_periodic_deadline(u64::MAX - 1, 2), None);
        assert_eq!(super::initial_periodic_deadline(u64::MAX - 1, 1), None);
    }

    #[test]
    fn periodic_deadline_saturates_at_the_monotonic_limit() {
        assert_eq!(
            super::next_periodic_deadline(u64::MAX - 5, u64::MAX - 1, 10),
            None
        );
        assert_eq!(
            super::next_periodic_deadline(u64::MAX - 5, u64::MAX, 10),
            None
        );
    }
}
