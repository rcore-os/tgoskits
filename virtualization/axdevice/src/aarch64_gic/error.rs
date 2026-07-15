//! Error translation at the Arm VGIC to AxDevice boundary.

use arm_vgic::VgicError;
use axdevice_base::{
    ControllerInputId, DeviceError, InterruptControllerId, InterruptEndpoint, IrqError,
};

use crate::DeviceManagerError;

pub(super) fn irq_error(
    controller: InterruptControllerId,
    input: Option<ControllerInputId>,
    operation: &'static str,
    error: VgicError,
) -> IrqError {
    let endpoint = input.map_or(InterruptEndpoint::Controller(controller), |input| {
        InterruptEndpoint::Wired { controller, input }
    });
    match error {
        VgicError::Unsupported { detail, .. } => IrqError::Unsupported {
            endpoint,
            operation,
            detail,
        },
        VgicError::Backend { detail, .. } => IrqError::Backend {
            endpoint,
            operation,
            detail,
        },
        error => IrqError::InvalidInput {
            endpoint,
            operation,
            detail: alloc::format!("{error}"),
        },
    }
}

pub(super) fn device_manager_error(error: VgicError) -> DeviceManagerError {
    match error {
        VgicError::InvalidConfig { detail } => DeviceManagerError::InvalidConfig {
            operation: "configure GICv3",
            detail,
        },
        VgicError::ResourceConflict { detail, .. } => DeviceManagerError::ResourceConflict {
            operation: "configure GICv3",
            detail,
        },
        VgicError::ResourceNotFound {
            operation,
            resource,
        } => DeviceManagerError::ResourceNotFound {
            operation,
            resource,
        },
        VgicError::Unsupported { operation, detail } => {
            DeviceManagerError::Unsupported { operation, detail }
        }
        error => DeviceManagerError::UnexpectedResponse {
            operation: "operate GICv3 controller",
            detail: alloc::format!("{error}"),
        },
    }
}

pub(super) fn device_error(error: VgicError) -> DeviceError {
    match error {
        VgicError::InvalidAccess {
            operation, detail, ..
        }
        | VgicError::InvalidStateTransition {
            operation, detail, ..
        } => DeviceError::InvalidInput { operation, detail },
        VgicError::Unsupported { operation, detail } => {
            DeviceError::Unsupported { operation, detail }
        }
        VgicError::Backend { operation, detail }
        | VgicError::GuestMemory {
            operation, detail, ..
        } => DeviceError::Backend { operation, detail },
        error => DeviceError::InvalidData {
            operation: "access GICv3",
            detail: alloc::format!("{error}"),
        },
    }
}
