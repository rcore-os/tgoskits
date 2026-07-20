use alloc::boxed::Box;

use rdif_irq::{IrqEndpoint, MaskedSource};

use crate::{DisplayError, DisplayInfo, DriverGeneric, FrameBuffer};

/// Runtime ownership required by one display controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayExecution {
    /// The device has no hardware completion source and completes in its owner call.
    Inline,
    /// The device exposes an interrupt endpoint that must be registered before activation.
    Interrupt,
}

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

/// Allocation-free fault classification emitted from a display IRQ endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DisplayIrqFault {
    /// The device returned an interrupt status that cannot be decoded safely.
    #[error("invalid display interrupt status")]
    InvalidStatus,
    /// The exact device source could not be masked after capture failed.
    #[error("display interrupt source could not be contained")]
    Uncontained,
}

/// Move-only interrupt endpoint owned by the registered OS callback.
pub type DisplayIrqEndpoint = Box<dyn IrqEndpoint<Event = Event, Fault = DisplayIrqFault>>;

pub trait Interface: DriverGeneric {
    /// Performs controller initialization on the final maintenance owner.
    fn initialize(&mut self) -> Result<(), DisplayError> {
        Ok(())
    }

    /// Declares whether activation requires a hardware interrupt source.
    fn execution(&self) -> DisplayExecution {
        DisplayExecution::Inline
    }

    fn info(&self) -> Result<DisplayInfo, DisplayError>;

    fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError>;

    fn need_flush(&self) -> bool {
        false
    }

    fn flush(&mut self) -> Result<(), DisplayError> {
        Ok(())
    }

    fn enable_irq(&mut self) -> Result<(), DisplayError> {
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), DisplayError> {
        Ok(())
    }

    /// Transfers the destructive IRQ-status endpoint to OS registration.
    fn take_irq_endpoint(&mut self) -> Option<DisplayIrqEndpoint> {
        None
    }

    /// Advances owner-only device state from one acknowledged IRQ snapshot.
    fn service_irq(&mut self, _event: Event) -> Result<(), DisplayError> {
        Ok(())
    }

    /// Rearms an exact source that capture deliberately left masked.
    ///
    /// Implementations must validate both the source bitmap and generation
    /// before touching hardware. A stale or foreign token must fail closed
    /// without re-enabling any device interrupt source.
    fn rearm_irq(&mut self, _source: MaskedSource) -> Result<(), DisplayError> {
        Err(DisplayError::NotSupported)
    }
}
