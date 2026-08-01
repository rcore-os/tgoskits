//! Durable status of the host GIC maintenance IRQ registration.

use irq_framework::IrqError;

/// Host-lifetime state exposed to later emulated-VGIC preparation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MaintenanceHandlerStatus {
    Uninitialized,
    Registered,
    Unavailable,
    Error(IrqError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MaintenanceHandlerRegistrationError {
    Unavailable,
    Error(IrqError),
}

pub(super) fn registration_status<T>(
    registration: Option<&Result<T, MaintenanceHandlerRegistrationError>>,
) -> MaintenanceHandlerStatus {
    match registration {
        None => MaintenanceHandlerStatus::Uninitialized,
        Some(Ok(_)) => MaintenanceHandlerStatus::Registered,
        Some(Err(MaintenanceHandlerRegistrationError::Unavailable)) => {
            MaintenanceHandlerStatus::Unavailable
        }
        Some(Err(MaintenanceHandlerRegistrationError::Error(error))) => {
            MaintenanceHandlerStatus::Error(*error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_every_host_lifetime_registration_state() {
        assert_eq!(
            registration_status::<()>(None),
            MaintenanceHandlerStatus::Uninitialized
        );
        assert_eq!(
            registration_status(Some(&Ok(()))),
            MaintenanceHandlerStatus::Registered
        );
        assert_eq!(
            registration_status::<()>(Some(&Err(MaintenanceHandlerRegistrationError::Unavailable))),
            MaintenanceHandlerStatus::Unavailable
        );
        assert_eq!(
            registration_status::<()>(Some(&Err(MaintenanceHandlerRegistrationError::Error(
                IrqError::InvalidCpu
            )))),
            MaintenanceHandlerStatus::Error(IrqError::InvalidCpu)
        );
    }
}
