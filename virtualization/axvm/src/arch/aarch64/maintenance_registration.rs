//! Durable status of the host GIC maintenance IRQ registration.

use irq_framework::{IrqCapabilityStatus, IrqError, IrqId};

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
    Uninitialized,
    Unavailable,
    Error(IrqError),
}

pub(super) fn maintenance_irq_from_status(
    status: IrqCapabilityStatus,
) -> Result<IrqId, MaintenanceHandlerRegistrationError> {
    match status {
        IrqCapabilityStatus::Available(irq) => Ok(irq),
        IrqCapabilityStatus::Uninitialized => {
            Err(MaintenanceHandlerRegistrationError::Uninitialized)
        }
        IrqCapabilityStatus::Unavailable => Err(MaintenanceHandlerRegistrationError::Unavailable),
        IrqCapabilityStatus::Error(error) => Err(MaintenanceHandlerRegistrationError::Error(error)),
    }
}

pub(super) fn registration_status<T>(
    registration: Option<&Result<T, MaintenanceHandlerRegistrationError>>,
) -> MaintenanceHandlerStatus {
    match registration {
        None => MaintenanceHandlerStatus::Uninitialized,
        Some(Ok(_)) => MaintenanceHandlerStatus::Registered,
        Some(Err(MaintenanceHandlerRegistrationError::Uninitialized)) => {
            MaintenanceHandlerStatus::Uninitialized
        }
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
    use irq_framework::{HwIrq, IrqDomainId};

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
            registration_status::<()>(Some(&Err(
                MaintenanceHandlerRegistrationError::Uninitialized
            ))),
            MaintenanceHandlerStatus::Uninitialized
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

    #[test]
    fn preserves_platform_discovery_status_at_the_registration_boundary() {
        let irq = IrqId::new(IrqDomainId(23), HwIrq(25));
        assert_eq!(
            maintenance_irq_from_status(IrqCapabilityStatus::Available(irq)),
            Ok(irq)
        );
        assert_eq!(
            maintenance_irq_from_status(IrqCapabilityStatus::Unavailable),
            Err(MaintenanceHandlerRegistrationError::Unavailable)
        );
        for error in [IrqError::Unsupported, IrqError::Busy] {
            let status = IrqCapabilityStatus::Error(error);
            let expected = Err(MaintenanceHandlerRegistrationError::Error(error));
            assert_eq!(maintenance_irq_from_status(status), expected);
            assert_eq!(maintenance_irq_from_status(status), expected);
        }
    }
}
