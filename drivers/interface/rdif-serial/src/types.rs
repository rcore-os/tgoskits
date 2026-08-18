use bitflags::bitflags;

bitflags! {
    /// Stable event classes exchanged between a UART and its runtime.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct SerialEventSet: u32 {
        const RX_DATA      = 1 << 0;
        const RX_TIMEOUT   = 1 << 1;
        const RX_STATUS    = 1 << 2;
        const TX_SPACE     = 1 << 3;
        const MODEM_STATUS = 1 << 4;
        const BUSY_DETECT  = 1 << 5;
        const FAULT        = 1 << 6;

        const RX = Self::RX_DATA.bits() | Self::RX_TIMEOUT.bits() | Self::RX_STATUS.bits();
    }
}

impl SerialEventSet {
    /// Returns whether any receive-side source is present.
    pub const fn has_rx(self) -> bool {
        self.intersects(Self::RX)
    }

    /// Returns whether the transmitter-space source is present.
    pub const fn has_tx(self) -> bool {
        self.contains(Self::TX_SPACE)
    }
}

bitflags! {
    /// RX error state reported by the IRQ endpoint while buffering samples.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct RxErrorFlags: u32 {
        const BREAK   = 1 << 0;
        const PARITY  = 1 << 1;
        const FRAMING = 1 << 2;
        const OVERRUN = 1 << 3;
    }
}

/// Stable event produced by an IRQ-owned UART endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialIrqEvent {
    pub events: SerialEventSet,
    pub rx_errors: RxErrorFlags,
    /// Sources masked by the IRQ endpoint and awaiting task-side rearm.
    pub rearm: SerialEventSet,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RxFlag {
    #[default]
    Normal,
    Break,
    Parity,
    Framing,
}

/// One hardware receive sample. Runtime channel policy is intentionally absent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxSample {
    pub byte: Option<u8>,
    pub flag: RxFlag,
    pub overrun: bool,
}

/// Maximum number of normalized RX samples returned by one hard-IRQ pass.
///
/// A full batch leaves the device source pending or reasserted so a later IRQ
/// can continue draining. Keeping the capacity in the portable value type makes
/// the hard-IRQ work and stack footprint independent of runtime policy.
pub const IRQ_RX_BATCH_CAPACITY: usize = 64;

const EMPTY_RX_SAMPLE: RxSample = RxSample {
    byte: None,
    flag: RxFlag::Normal,
    overrun: false,
};

/// Fixed-capacity RX data extracted by one UART hard-IRQ pass.
///
/// The driver owns construction and the runtime owns publication into its
/// preallocated queue. No callback into OS code runs while the driver holds or
/// reads device registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IrqRxBatch {
    samples: [RxSample; IRQ_RX_BATCH_CAPACITY],
    len: usize,
}

impl IrqRxBatch {
    /// Creates an empty fixed-capacity batch.
    pub const fn new() -> Self {
        Self {
            samples: [EMPTY_RX_SAMPLE; IRQ_RX_BATCH_CAPACITY],
            len: 0,
        }
    }

    /// Appends one sample or returns it unchanged when the fixed batch is full.
    pub fn try_push(&mut self, sample: RxSample) -> Result<(), RxSample> {
        let Some(slot) = self.samples.get_mut(self.len) else {
            return Err(sample);
        };
        *slot = sample;
        self.len += 1;
        Ok(())
    }

    /// Returns the number of buffered samples.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the batch contains no samples.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Borrows the initialized prefix of the batch.
    pub fn as_slice(&self) -> &[RxSample] {
        &self.samples[..self.len]
    }
}

impl Default for IrqRxBatch {
    fn default() -> Self {
        Self::new()
    }
}

/// Complete value returned by one UART hard-IRQ pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialIrqReport {
    pub event: SerialIrqEvent,
    pub rx: IrqRxBatch,
}

impl SerialIrqReport {
    /// Combines one normalized IRQ event with its bounded receive batch.
    pub const fn new(event: SerialIrqEvent, rx: IrqRxBatch) -> Self {
        Self { event, rx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irq_rx_batch_rejects_samples_beyond_its_fixed_capacity() {
        let mut batch = IrqRxBatch::new();
        let sample = RxSample {
            byte: Some(b'x'),
            ..RxSample::default()
        };

        for _ in 0..IRQ_RX_BATCH_CAPACITY {
            assert_eq!(batch.try_push(sample), Ok(()));
        }

        assert_eq!(batch.try_push(sample), Err(sample));
        assert_eq!(batch.len(), IRQ_RX_BATCH_CAPACITY);
        assert_eq!(batch.as_slice(), &[sample; IRQ_RX_BATCH_CAPACITY]);
    }
}
