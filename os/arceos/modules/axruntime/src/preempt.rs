//! Architecture preemption adapter and scheduler safe-point baton.

#[cfg(feature = "irq")]
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(feature = "irq")]
#[ax_percpu::def_percpu]
static SCHEDULER_BATON: AtomicBool = AtomicBool::new(false);

struct RuntimePreemptionOps;

#[ax_crate_interface::impl_interface]
impl ax_task::runtime_preempt::RuntimePreemption for RuntimePreemptionOps {
    fn enter() -> usize {
        cpu_local::enter_preemption().into_raw()
    }

    fn exit(token: usize) {
        exit_preemption(token);
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

    fn finish_initial_context_switch() {
        debug_assert!(!ax_hal::asm::irqs_enabled());
        with_cpu_pin(cpu_local::release_initial_context_preemption)
            .unwrap_or_else(|error| panic!("initial preemption handoff is invalid: {error}"));
    }
}

pub(crate) fn release_bootstrap() {
    with_cpu_pin(cpu_local::release_bootstrap_preemption)
        .unwrap_or_else(|error| panic!("bootstrap preemption state is invalid: {error}"));
}

fn exit_preemption(raw: usize) {
    // SAFETY: axtask transports the exact opaque value returned by `enter`
    // through one non-Send guard and consumes it here exactly once.
    let token = unsafe { cpu_local::PreemptionToken::from_raw(raw) }
        .expect("runtime preemption token must retain its aligned owner");
    let irqs_were_enabled = ax_hal::asm::irqs_enabled();
    ax_hal::asm::disable_irqs();

    let token = with_cpu_pin(|pin| cpu_local::handoff_preemption_after_context_switch(pin, token))
        .unwrap_or_else(|error| panic!("context-switch preemption handoff failed: {error}"));

    let pending = ax_task::runtime_preemption_pending();
    with_cpu_pin(|pin| {
        if pending {
            cpu_local::set_preemption_pending(pin)
        } else {
            cpu_local::clear_preemption_pending(pin)
        }
    })
    .unwrap_or_else(|error| panic!("runtime preemption publication failed: {error}"));

    match cpu_local::finish_preemption(token) {
        cpu_local::PreemptionExit::Nested | cpu_local::PreemptionExit::Enabled => {}
        cpu_local::PreemptionExit::Pending(pending) => {
            claim_scheduler_baton();
            pending.release();
            with_cpu_pin(cpu_local::clear_preemption_pending).unwrap_or_else(|error| {
                panic!("runtime preemption clear failed before scheduling: {error}")
            });
            release_scheduler_baton();
            ax_task::runtime_preempt_current();
        }
    }

    if irqs_were_enabled {
        ax_hal::asm::enable_irqs();
    }
}

#[cfg(feature = "irq")]
fn claim_scheduler_baton() {
    with_cpu_pin(|pin| {
        SCHEDULER_BATON.with_current(pin, |baton| {
            assert!(
                baton
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok(),
                "runtime scheduler baton is already active"
            );
        })
    });
}

#[cfg(not(feature = "irq"))]
fn claim_scheduler_baton() {
    panic!("pending preemption requires the runtime IRQ capability");
}

#[cfg(feature = "irq")]
fn release_scheduler_baton() {
    with_cpu_pin(|pin| {
        SCHEDULER_BATON.with_current(pin, |baton| {
            assert!(
                baton
                    .compare_exchange(true, false, Ordering::Release, Ordering::Relaxed)
                    .is_ok(),
                "runtime scheduler baton was not active"
            );
        })
    });
}

#[cfg(not(feature = "irq"))]
fn release_scheduler_baton() {
    unreachable!("a runtime without IRQ capability cannot claim the baton");
}

fn with_cpu_pin<R>(operation: impl for<'scope> FnOnce(&cpu_local::CpuPin<'scope>) -> R) -> R {
    // SAFETY: callers mask local IRQs for the complete operation, so this
    // execution cannot migrate while the scoped CPU capability is live.
    unsafe { cpu_local::with_cpu_pin(operation) }
        .unwrap_or_else(|error| panic!("runtime CPU-local state is invalid: {error}"))
}
