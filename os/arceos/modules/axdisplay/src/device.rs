use alloc::{boxed::Box, string::String};

use crate::DisplayInfo;

pub type DisplayResult<T = ()> = Result<T, DisplayError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayError {
    NotSupported,
    NotAvailable,
    InvalidFramebuffer,
    BadState,
}

/// Domain boundary consumed by graphics modules and device files.
pub trait DisplayDevice: Send {
    fn name(&self) -> &str;

    fn info(&self) -> DisplayInfo;

    fn flush(&mut self) -> DisplayResult;

    fn irq_num(&self) -> Option<usize> {
        None
    }

    fn enable_irq(&mut self) {}

    fn disable_irq(&mut self) {}

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn handle_irq(&mut self) -> bool {
        false
    }
}

pub struct ErasedDisplayDevice {
    name: String,
    inner: Box<dyn DisplayDevice>,
}

impl ErasedDisplayDevice {
    pub fn new(device: impl DisplayDevice + 'static) -> Self {
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

impl DisplayDevice for ErasedDisplayDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn info(&self) -> DisplayInfo {
        self.inner.info()
    }

    fn flush(&mut self) -> DisplayResult {
        self.inner.flush()
    }

    fn irq_num(&self) -> Option<usize> {
        self.inner.irq_num()
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

    fn handle_irq(&mut self) -> bool {
        self.inner.handle_irq()
    }
}
