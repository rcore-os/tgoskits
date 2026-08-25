//! Architecture preemption adapter and scheduler safe-point frame.

use core::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SchedulerBatonState {
    /// The final pending preemption depth has become the runtime baton, but the
    /// task layer has not entered its scheduler frame yet.
    PreemptEntry,
    Active,
    Transferred,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreemptionExitOrigin {
    Task,
    IrqReturn,
}

#[ax_percpu::def_percpu]
static SCHEDULER_BATON: AtomicU8 = AtomicU8::new(SchedulerBatonState::Finished as u8);

struct RuntimePreemptionOps;

#[ax_crate_interface::impl_interface]
impl ax_task::runtime_preempt::RuntimePreemption for RuntimePreemptionOps {
    fn enter() -> usize {
        cpu_local::enter_preemption().into_raw()
    }

    fn exit(token: usize) {
        exit_preemption(token, PreemptionExitOrigin::Task);
    }

    fn exit_from_irq_return(token: usize) {
        exit_preemption(token, PreemptionExitOrigin::IrqReturn);
    }

    fn depth() -> usize {
        let irqs_were_enabled = ax_hal::asm::irqs_enabled();
        ax_hal::asm::disable_irqs();
        let depth =
            with_cpu_pin(|pin| cpu_local::preemption_snapshot(pin).map(|state| state.depth()))
                .unwrap_or_else(|error| panic!("runtime preemption state is invalid: {error}"));
        if irqs_were_enabled {
            ax_hal::asm::enable_irqs();
        }
        depth as usize
    }

    fn enter_scheduler_frame() {
        enter_scheduler_frame();
    }

    fn transfer_scheduler_frame() {
        transfer_scheduler_frame();
    }

    fn finish_scheduler_frame(result: ax_task::runtime_preempt::SchedulerFrameResult) {
        finish_scheduler_frame(result);
    }

    fn finish_initial_context_switch() {
        debug_assert!(!ax_hal::asm::irqs_enabled());
        with_cpu_pin(cpu_local::release_initial_context_preemption)
            .unwrap_or_else(|error| panic!("initial preemption handoff is invalid: {error}"));
        publish_current_preemption_pending();
        finish_initial_scheduler_baton();
        debug_assert!(
            !ax_hal::asm::irqs_enabled(),
            "first-entry scheduler frame must finish before IRQ enable"
        );
    }
}

pub(crate) fn release_bootstrap() {
    with_cpu_pin(cpu_local::release_bootstrap_preemption)
        .unwrap_or_else(|error| panic!("bootstrap preemption state is invalid: {error}"));
}

fn exit_preemption(raw: usize, origin: PreemptionExitOrigin) {
    // SAFETY: axtask transports the exact opaque value returned by `enter`
    // through one non-Send guard and consumes it here exactly once.
    let token = unsafe { cpu_local::PreemptionToken::from_raw(raw) }
        .expect("runtime preemption token must retain its aligned owner");
    let irqs_were_enabled = ax_hal::asm::irqs_enabled();
    assert!(
        origin != PreemptionExitOrigin::IrqReturn || !irqs_were_enabled,
        "IRQ-return preemption exit requires hardware IRQs disabled"
    );
    ax_hal::asm::disable_irqs();

    let token = with_cpu_pin(|pin| cpu_local::handoff_preemption_after_context_switch(pin, token))
        .unwrap_or_else(|error| panic!("context-switch preemption handoff failed: {error}"));

    publish_current_preemption_pending();

    match cpu_local::finish_preemption(token) {
        cpu_local::PreemptionExit::Nested | cpu_local::PreemptionExit::Enabled => {}
        cpu_local::PreemptionExit::Pending(pending) => {
            let should_schedule = preemption_exit_should_schedule(origin, irqs_were_enabled);
            if should_schedule {
                claim_preempt_scheduler_entry();
            }
            pending.release();
            if should_schedule {
                with_cpu_pin(cpu_local::clear_preemption_pending).unwrap_or_else(|error| {
                    panic!("runtime preemption clear failed before scheduling: {error}")
                });
                ax_task::runtime_preempt_current();
                finish_unused_preempt_scheduler_entry();
            }
        }
    }

    if irqs_were_enabled {
        ax_hal::asm::enable_irqs();
    }
}

const fn preemption_exit_should_schedule(
    origin: PreemptionExitOrigin,
    irqs_were_enabled: bool,
) -> bool {
    matches!(origin, PreemptionExitOrigin::IrqReturn) || irqs_were_enabled
}

fn claim_preempt_scheduler_entry() {
    debug_assert!(!ax_hal::asm::irqs_enabled());
    transition_scheduler_baton(
        |state| match state {
            SchedulerBatonState::Finished => Some(SchedulerBatonState::PreemptEntry),
            _ => None,
        },
        "pending preemption requires a finished scheduler baton",
    );
}

fn enter_scheduler_frame() {
    debug_assert!(!ax_hal::asm::irqs_enabled());
    transition_scheduler_baton(
        |state| match state {
            SchedulerBatonState::Finished | SchedulerBatonState::PreemptEntry => {
                Some(SchedulerBatonState::Active)
            }
            _ => None,
        },
        "scheduler entry requires a finished or preclaimed baton",
    );
}

fn transfer_scheduler_frame() {
    debug_assert!(!ax_hal::asm::irqs_enabled());
    transition_scheduler_baton(
        |state| (state == SchedulerBatonState::Active).then_some(SchedulerBatonState::Transferred),
        "raw context switch requires the active scheduler baton",
    );
}

fn finish_scheduler_frame(result: ax_task::runtime_preempt::SchedulerFrameResult) {
    use ax_task::runtime_preempt::SchedulerFrameResult;

    debug_assert!(!ax_hal::asm::irqs_enabled());
    publish_current_preemption_pending();
    match result {
        SchedulerFrameResult::Stayed => {
            finish_scheduler_baton(SchedulerBatonState::Active, "same-context scheduler return")
        }
        SchedulerFrameResult::Resumed => finish_scheduler_baton(
            SchedulerBatonState::Transferred,
            "resumed scheduler continuation",
        ),
    }
}

fn finish_initial_scheduler_baton() {
    finish_scheduler_baton(
        SchedulerBatonState::Transferred,
        "initial context-switch tail",
    );
}

fn finish_unused_preempt_scheduler_entry() {
    debug_assert!(!ax_hal::asm::irqs_enabled());
    publish_current_preemption_pending();
    with_scheduler_baton(|baton| {
        if scheduler_baton_state(baton) == SchedulerBatonState::PreemptEntry {
            transition_scheduler_baton_value(
                baton,
                |state| {
                    (state == SchedulerBatonState::PreemptEntry)
                        .then_some(SchedulerBatonState::Finished)
                },
                "unused pending-preemption scheduler entry",
            );
        }
    });
}

fn finish_scheduler_baton(expected: SchedulerBatonState, owner: &'static str) {
    transition_scheduler_baton(
        |state| (state == expected).then_some(SchedulerBatonState::Finished),
        owner,
    );
}

fn transition_scheduler_baton(
    transition: impl FnOnce(SchedulerBatonState) -> Option<SchedulerBatonState>,
    invariant: &'static str,
) {
    with_scheduler_baton(|baton| transition_scheduler_baton_value(baton, transition, invariant));
}

fn transition_scheduler_baton_value(
    baton: &AtomicU8,
    transition: impl FnOnce(SchedulerBatonState) -> Option<SchedulerBatonState>,
    invariant: &'static str,
) {
    let current_raw = baton.load(Ordering::Relaxed);
    let current = SchedulerBatonState::from_raw(current_raw);
    let next =
        transition(current).unwrap_or_else(|| panic!("{invariant}; current state is {current:?}"));
    assert_eq!(
        baton.compare_exchange(
            current_raw,
            next as u8,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ),
        Ok(current_raw),
        "scheduler baton changed despite local IRQ exclusion"
    );
}

fn scheduler_baton_state(baton: &AtomicU8) -> SchedulerBatonState {
    SchedulerBatonState::from_raw(baton.load(Ordering::Relaxed))
}

impl SchedulerBatonState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            raw if raw == Self::PreemptEntry as u8 => Self::PreemptEntry,
            raw if raw == Self::Active as u8 => Self::Active,
            raw if raw == Self::Transferred as u8 => Self::Transferred,
            raw if raw == Self::Finished as u8 => Self::Finished,
            _ => panic!("invalid scheduler baton state {raw}"),
        }
    }
}

fn with_scheduler_baton<R>(operation: impl FnOnce(&AtomicU8) -> R) -> R {
    with_cpu_pin(|pin| SCHEDULER_BATON.with_current(pin, operation))
}

fn publish_current_preemption_pending() {
    let pending = ax_task::runtime_preemption_pending();
    with_cpu_pin(|pin| {
        if pending {
            cpu_local::set_preemption_pending(pin)
        } else {
            cpu_local::clear_preemption_pending(pin)
        }
    })
    .unwrap_or_else(|error| panic!("runtime preemption publication failed: {error}"));
}

fn with_cpu_pin<R>(operation: impl for<'scope> FnOnce(&cpu_local::CpuPin<'scope>) -> R) -> R {
    // SAFETY: callers mask local IRQs for the complete operation, so this
    // execution cannot migrate while the scoped CPU capability is live.
    unsafe { cpu_local::with_cpu_pin(operation) }
        .unwrap_or_else(|error| panic!("runtime CPU-local state is invalid: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transition(
        baton: &AtomicU8,
        transition: impl FnOnce(SchedulerBatonState) -> Option<SchedulerBatonState>,
    ) {
        transition_scheduler_baton_value(baton, transition, "test transition");
    }

    #[test]
    fn pending_preemption_enters_one_scheduler_frame() {
        let baton = AtomicU8::new(SchedulerBatonState::Finished as u8);
        transition(&baton, |state| {
            (state == SchedulerBatonState::Finished).then_some(SchedulerBatonState::PreemptEntry)
        });
        transition(&baton, |state| {
            (state == SchedulerBatonState::PreemptEntry).then_some(SchedulerBatonState::Active)
        });

        assert_eq!(scheduler_baton_state(&baton), SchedulerBatonState::Active);
    }

    #[test]
    fn raw_switch_transfers_and_first_entry_consumes_baton() {
        let baton = AtomicU8::new(SchedulerBatonState::Active as u8);
        transition(&baton, |state| {
            (state == SchedulerBatonState::Active).then_some(SchedulerBatonState::Transferred)
        });
        transition(&baton, |state| {
            (state == SchedulerBatonState::Transferred).then_some(SchedulerBatonState::Finished)
        });

        assert_eq!(scheduler_baton_state(&baton), SchedulerBatonState::Finished);
    }

    #[test]
    #[should_panic(expected = "test transition; current state is Active")]
    fn scheduler_baton_cannot_be_claimed_twice() {
        let baton = AtomicU8::new(SchedulerBatonState::Active as u8);
        transition(&baton, |state| {
            (state == SchedulerBatonState::Finished).then_some(SchedulerBatonState::Active)
        });
    }

    #[test]
    fn only_irq_return_may_schedule_from_an_irq_disabled_preemption_exit() {
        assert!(!preemption_exit_should_schedule(
            PreemptionExitOrigin::Task,
            false
        ));
        assert!(preemption_exit_should_schedule(
            PreemptionExitOrigin::Task,
            true
        ));
        assert!(preemption_exit_should_schedule(
            PreemptionExitOrigin::IrqReturn,
            false
        ));
    }
}
