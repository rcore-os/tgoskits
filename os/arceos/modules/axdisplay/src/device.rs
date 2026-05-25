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
}
