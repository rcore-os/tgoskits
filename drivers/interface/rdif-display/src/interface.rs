use crate::{DisplayError, DisplayInfo, DriverGeneric, FrameBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub handled: bool,
    pub changed: bool,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            handled: false,
            changed: false,
        }
    }
}

pub trait Interface: DriverGeneric {
    fn info(&self) -> DisplayInfo;

    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError>;

    fn irq_num(&self) -> Option<usize> {
        None
    }

    fn need_flush(&self) -> bool {
        false
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        Ok(())
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
