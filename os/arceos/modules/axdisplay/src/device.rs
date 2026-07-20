//! Immutable display publication and remote flush capability.

use alloc::{string::String, sync::Arc};

use crate::DisplayInfo;

pub type DisplayResult<T = ()> = Result<T, DisplayError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayError {
    NotSupported,
    NotAvailable,
    InvalidFramebuffer,
    BadState,
}

/// Runtime service that submits one flush to the display maintenance owner.
pub trait DisplayFlushService: Send + Sync {
    /// Completes after the owner has accepted and executed this flush.
    fn flush(&self) -> DisplayResult;
}

/// Read-only framebuffer publication plus its owner-thread command facade.
pub struct DisplayFacade {
    name: String,
    info: DisplayInfo,
    flush: Arc<dyn DisplayFlushService>,
}

impl DisplayFacade {
    /// Creates one fully activated display publication.
    pub fn new(
        name: impl Into<String>,
        info: DisplayInfo,
        flush: Arc<dyn DisplayFlushService>,
    ) -> Self {
        Self {
            name: name.into(),
            info,
            flush,
        }
    }

    /// Returns the stable device name captured at activation.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the immutable framebuffer layout captured at activation.
    pub const fn info(&self) -> DisplayInfo {
        self.info
    }

    /// Routes one flush to the device's fixed maintenance owner.
    pub fn flush(&self) -> DisplayResult {
        self.flush.flush()
    }
}
