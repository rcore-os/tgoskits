use crate::{AbsInfo, DriverGeneric, EventType, InputDeviceId, InputError, InputEvent};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Event {
    pub input_ready: bool,
}

impl Event {
    pub const fn none() -> Self {
        Self { input_ready: false }
    }
}

pub trait Interface: DriverGeneric {
    fn device_id(&self) -> InputDeviceId;

    fn physical_location(&self) -> &str;

    fn unique_id(&self) -> &str;

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> Result<bool, InputError>;

    fn read_event(&mut self) -> Result<InputEvent, InputError>;

    fn get_prop_bits(&mut self, _out: &mut [u8]) -> Result<usize, InputError> {
        Ok(0)
    }

    fn get_abs_info(&mut self, _axis: u8) -> Result<AbsInfo, InputError> {
        Err(InputError::NotSupported)
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> Event {
        Event::none()
    }
}
