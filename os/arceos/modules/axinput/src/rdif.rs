use alloc::{boxed::Box, string::String};

use irq_framework::IrqId;
use rdif_input::{InputError as RdifInputError, Interface};

use crate::{
    AbsInfo, Event, EventType, InputDevice, InputDeviceId, InputError, InputIrqEvent, InputResult,
};

pub struct RdifInputDevice {
    name: String,
    device: Box<dyn Interface>,
    irq: Option<IrqId>,
}

impl RdifInputDevice {
    pub fn new(device: Box<dyn Interface>) -> Self {
        Self::new_with_irq(device, None)
    }

    pub fn new_with_irq(device: Box<dyn Interface>, irq: Option<IrqId>) -> Self {
        let name = device.name().into();
        Self { name, device, irq }
    }

    pub fn from_interface(device: impl Interface + 'static) -> Self {
        Self::new(Box::new(device))
    }
}

impl InputDevice for RdifInputDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn device_id(&self) -> InputDeviceId {
        self.device.device_id().into()
    }

    fn physical_location(&self) -> &str {
        self.device.physical_location()
    }

    fn unique_id(&self) -> &str {
        self.device.unique_id()
    }

    fn irq_id(&self) -> Option<IrqId> {
        self.irq
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> InputResult<bool> {
        self.device
            .get_event_bits(ty.into(), out)
            .map_err(map_input_error)
    }

    fn read_event(&mut self) -> InputResult<Event> {
        self.device
            .read_event()
            .map(Into::into)
            .map_err(map_input_error)
    }

    fn get_prop_bits(&mut self, out: &mut [u8]) -> InputResult<usize> {
        self.device.get_prop_bits(out).map_err(map_input_error)
    }

    fn get_abs_info(&mut self, axis: u8) -> InputResult<AbsInfo> {
        self.device
            .get_abs_info(axis)
            .map(Into::into)
            .map_err(map_input_error)
    }

    fn enable_irq(&mut self) {
        self.device.enable_irq();
    }

    fn disable_irq(&mut self) {
        self.device.disable_irq();
    }

    fn is_irq_enabled(&self) -> bool {
        self.device.is_irq_enabled()
    }

    fn handle_irq(&mut self) -> InputIrqEvent {
        let event = self.device.handle_irq();
        InputIrqEvent {
            handled: event.handled,
            input_ready: event.input_ready,
        }
    }
}

impl From<rdif_input::InputDeviceId> for InputDeviceId {
    fn from(value: rdif_input::InputDeviceId) -> Self {
        Self {
            bus_type: value.bus_type,
            vendor: value.vendor,
            product: value.product,
            version: value.version,
        }
    }
}

#[cfg(test)]
mod tests {
    use irq_framework::{HwIrq, IrqDomainId, IrqId};
    use rdif_input::{EventType, InputDeviceId, InputError, InputEvent};

    use super::*;

    struct TestInput;

    impl rdif_input::DriverGeneric for TestInput {
        fn name(&self) -> &str {
            "test-input"
        }
    }

    impl Interface for TestInput {
        fn device_id(&self) -> InputDeviceId {
            InputDeviceId {
                bus_type: 3,
                vendor: 1,
                product: 2,
                version: 1,
            }
        }

        fn physical_location(&self) -> &str {
            "test/input0"
        }

        fn unique_id(&self) -> &str {
            "input0"
        }

        fn get_event_bits(&mut self, _ty: EventType, _out: &mut [u8]) -> Result<bool, InputError> {
            Ok(false)
        }

        fn read_event(&mut self) -> Result<InputEvent, InputError> {
            Err(InputError::Again)
        }
    }

    #[test]
    fn rdif_input_device_exposes_resolved_irq_id() {
        let irq = IrqId::new(IrqDomainId(7), HwIrq(42));
        let device = RdifInputDevice::new_with_irq(Box::new(TestInput), Some(irq));
        let erased = crate::ErasedInputDevice::new(device);

        assert_eq!(erased.irq_id(), Some(irq));
    }

    #[test]
    fn rdif_input_device_without_resolved_irq_has_no_irq_id() {
        let device = RdifInputDevice::new(Box::new(TestInput));
        let erased = crate::ErasedInputDevice::new(device);

        assert_eq!(erased.irq_id(), None);
    }
}

impl From<EventType> for rdif_input::EventType {
    fn from(value: EventType) -> Self {
        match value {
            EventType::Synchronization => Self::Synchronization,
            EventType::Key => Self::Key,
            EventType::Relative => Self::Relative,
            EventType::Absolute => Self::Absolute,
            EventType::Misc => Self::Misc,
            EventType::Switch => Self::Switch,
            EventType::Led => Self::Led,
            EventType::Sound => Self::Sound,
            EventType::ForceFeedback => Self::ForceFeedback,
        }
    }
}

impl From<rdif_input::InputEvent> for Event {
    fn from(value: rdif_input::InputEvent) -> Self {
        Self {
            event_type: value.event_type,
            code: value.code,
            value: value.value,
        }
    }
}

impl From<rdif_input::AbsInfo> for AbsInfo {
    fn from(value: rdif_input::AbsInfo) -> Self {
        Self {
            min: value.min,
            max: value.max,
            fuzz: value.fuzz,
            flat: value.flat,
            res: value.res,
        }
    }
}

fn map_input_error(error: RdifInputError) -> InputError {
    match error {
        RdifInputError::NotSupported => InputError::Unsupported,
        RdifInputError::Again => InputError::Again,
        RdifInputError::NotAvailable => InputError::ResourceBusy,
        RdifInputError::InvalidEvent => InputError::InvalidInput,
        RdifInputError::Other(_) => InputError::Io,
    }
}
