//! Task-context completion for one fixed hard-IRQ service waiter.

use ax_std::os::arceos::task::{self as scheduler, IrqRegisterResult, IrqWaitToken, TaskError};

/// Completes one register/park/fan-out cycle without exposing IRQ-owned
/// registration storage to a device implementation.
///
/// A notification consumed during registration is already detached and runs
/// the task-context fan-out immediately. A published token remains alive until
/// both cell ownership and any in-flight hard-IRQ wake have ended.
pub(super) fn complete_irq_service_cycle<'cell, P, F>(
    registration: IrqRegisterResult<'cell>,
    park: P,
    fanout: F,
) -> Result<bool, TaskError>
where
    P: FnOnce(&IrqWaitToken<'cell>),
    F: FnOnce(),
{
    match registration {
        IrqRegisterResult::ConsumedPending => {
            fanout();
            Ok(true)
        }
        IrqRegisterResult::Registered(token) | IrqRegisterResult::NotificationInFlight(token) => {
            park(&token);
            scheduler::quiesce_irq_wait(token)?;
            fanout();
            Ok(true)
        }
        IrqRegisterResult::Occupied => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[test]
    fn pending_before_register_fans_out_without_parking() {
        let step = Cell::new(0);
        let completed = complete_irq_service_cycle(
            IrqRegisterResult::ConsumedPending,
            |_| panic!("a synchronously consumed IRQ must not park"),
            || {
                assert_eq!(step.get(), 0);
                step.set(1);
            },
        )
        .unwrap();

        assert!(completed);
        assert_eq!(step.get(), 1);
    }

    #[test]
    fn occupied_registration_does_not_run_callbacks() {
        let step = Cell::new(0);
        let completed = complete_irq_service_cycle(
            IrqRegisterResult::Occupied,
            |_| step.set(1),
            || step.set(2),
        )
        .unwrap();

        assert!(!completed);
        assert_eq!(step.get(), 0);
    }
}
