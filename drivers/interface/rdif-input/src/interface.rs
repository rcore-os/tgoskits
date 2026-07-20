use alloc::boxed::Box;

use rdif_irq::{IrqEndpoint, MaskedSource};

use crate::{AbsInfo, DriverGeneric, EventType, InputDeviceId, InputError, InputEvent};

/// Runtime ownership required by an input source.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InputExecution {
    /// A software source never registers a hardware IRQ endpoint.
    Inline,
    /// A hardware source requires one detached IRQ endpoint and owner thread.
    Interrupt,
}

/// Stable input-controller facts captured while acknowledging one IRQ.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct IrqEvent {
    pub handled: bool,
    pub input_ready: bool,
}

impl IrqEvent {
    pub const fn none() -> Self {
        Self {
            handled: false,
            input_ready: false,
        }
    }
}

/// Allocation-free fault emitted by an input IRQ endpoint.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum InputIrqFault {
    /// The captured interrupt status cannot be decoded safely.
    #[error("invalid input interrupt status")]
    InvalidStatus,
    /// The endpoint could not mask its precise hardware source.
    #[error("input interrupt source could not be contained")]
    Uncontained,
}

/// Move-only destructive interrupt endpoint transferred to OS registration.
pub type InputIrqEndpoint = Box<dyn IrqEndpoint<Event = IrqEvent, Fault = InputIrqFault>>;

pub trait Interface: DriverGeneric {
    /// Initializes the device on its final CPU-pinned maintenance owner.
    fn initialize(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    /// Declares whether activation requires a hardware interrupt source.
    fn execution(&self) -> InputExecution {
        InputExecution::Inline
    }

    fn device_id(&self) -> InputDeviceId;

    fn physical_location(&self) -> &str;

    fn unique_id(&self) -> &str;

    fn irq_num(&self) -> Option<usize> {
        None
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError>;

    fn read_event(&mut self) -> Result<InputEvent, InputError>;

    fn get_prop_bits(&mut self, _out: &mut [u8]) -> Result<usize, InputError> {
        Ok(0)
    }

    fn get_abs_info(&mut self, _axis: u8) -> Result<AbsInfo, InputError> {
        Err(InputError::NotSupported)
    }

    /// Enables the device-side interrupt source after the OS action is live.
    fn enable_irq(&mut self) -> Result<(), InputError>;

    /// Masks the device-side source before the OS action is disabled.
    fn disable_irq(&mut self) -> Result<(), InputError>;

    fn is_irq_enabled(&self) -> bool;

    /// Transfers destructive IRQ status ownership to the OS action.
    fn take_irq_endpoint(&mut self) -> Option<InputIrqEndpoint> {
        None
    }

    /// Rearms one generation-checked source masked by the IRQ endpoint.
    fn rearm_irq(&mut self, _source: MaskedSource) -> Result<(), InputError> {
        Err(InputError::NotSupported)
    }
}
