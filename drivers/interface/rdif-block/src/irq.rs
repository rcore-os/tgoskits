/// Fixed-width queue set published from hard IRQ context without allocation.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqQueueMask(u64);

impl IrqQueueMask {
    pub const fn none() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn from_queue(queue_id: usize) -> Self {
        if queue_id < u64::BITS as usize {
            Self(1_u64 << queue_id)
        } else {
            Self::none()
        }
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn contains(self, queue_id: usize) -> bool {
        queue_id < u64::BITS as usize && self.0 & (1_u64 << queue_id) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqDisposition {
    Spurious,
    Cleared,
    MaskedNeedsRearm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqAck {
    disposition: IrqDisposition,
    queues: IrqQueueMask,
    control_event: crate::ControlEvent,
}

impl IrqAck {
    pub const fn spurious(source_id: usize) -> Self {
        Self {
            disposition: IrqDisposition::Spurious,
            queues: IrqQueueMask::none(),
            control_event: crate::ControlEvent::new(source_id, 0),
        }
    }

    pub const fn cleared(queues: IrqQueueMask, control_event: crate::ControlEvent) -> Self {
        Self {
            disposition: IrqDisposition::Cleared,
            queues,
            control_event,
        }
    }

    pub const fn masked_needs_rearm(
        queues: IrqQueueMask,
        control_event: crate::ControlEvent,
    ) -> Self {
        Self {
            disposition: IrqDisposition::MaskedNeedsRearm,
            queues,
            control_event,
        }
    }

    pub const fn disposition(self) -> IrqDisposition {
        self.disposition
    }

    pub const fn queues(self) -> IrqQueueMask {
        self.queues
    }

    pub const fn control_event(self) -> crate::ControlEvent {
        self.control_event
    }

    pub const fn is_spurious(self) -> bool {
        matches!(self.disposition, IrqDisposition::Spurious)
    }
}

/// Minimal device-local hard IRQ top half.
pub trait HardIrqHandler: Send + 'static {
    /// Identifies and acknowledges one fixed source without allocation,
    /// completion draining, DMA copies, registry access, or task scheduling.
    fn ack(&mut self) -> IrqAck;
}
