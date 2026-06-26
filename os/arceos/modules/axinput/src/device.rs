use alloc::{boxed::Box, string::String};

use irq_framework::IrqId;

use crate::{AbsInfo, Event, EventType, InputDeviceId};

pub type InputResult<T = ()> = Result<T, InputError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    AlreadyExists,
    Again,
    BadState,
    InvalidInput,
    Io,
    NoMemory,
    ResourceBusy,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct InputIrqEvent {
    pub handled: bool,
    pub input_ready: bool,
}

impl InputIrqEvent {
    pub const fn none() -> Self {
        Self {
            handled: false,
            input_ready: false,
        }
    }
}

/// Domain boundary consumed by evdev and upper input services.
pub trait InputDevice: Send {
    fn name(&self) -> &str;

    fn device_id(&self) -> InputDeviceId;

    fn physical_location(&self) -> &str;

    fn unique_id(&self) -> &str;

    fn irq_id(&self) -> Option<IrqId> {
        None
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> InputResult<bool>;

    fn read_event(&mut self) -> InputResult<Event>;

    fn get_prop_bits(&mut self, _out: &mut [u8]) -> InputResult<usize> {
        Ok(0)
    }

    fn get_abs_info(&mut self, _axis: u8) -> InputResult<AbsInfo> {
        Err(InputError::Unsupported)
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> InputIrqEvent {
        InputIrqEvent::none()
    }
}

pub struct ErasedInputDevice {
    name: String,
    inner: Box<dyn InputDevice>,
}

impl ErasedInputDevice {
    pub fn new(device: impl InputDevice + 'static) -> Self {
        let name = device.name().into();
        Self {
            name,
            inner: Box::new(device),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl InputDevice for ErasedInputDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn device_id(&self) -> InputDeviceId {
        self.inner.device_id()
    }

    fn physical_location(&self) -> &str {
        self.inner.physical_location()
    }

    fn unique_id(&self) -> &str {
        self.inner.unique_id()
    }

    fn irq_id(&self) -> Option<IrqId> {
        self.inner.irq_id()
    }

    fn get_event_bits(&mut self, ty: EventType, out: &mut [u8]) -> InputResult<bool> {
        self.inner.get_event_bits(ty, out)
    }

    fn read_event(&mut self) -> InputResult<Event> {
        self.inner.read_event()
    }

    fn get_prop_bits(&mut self, out: &mut [u8]) -> InputResult<usize> {
        self.inner.get_prop_bits(out)
    }

    fn get_abs_info(&mut self, axis: u8) -> InputResult<AbsInfo> {
        self.inner.get_abs_info(axis)
    }

    fn enable_irq(&mut self) {
        self.inner.enable_irq();
    }

    fn disable_irq(&mut self) {
        self.inner.disable_irq();
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.is_irq_enabled()
    }

    fn handle_irq(&mut self) -> InputIrqEvent {
        self.inner.handle_irq()
    }
}
