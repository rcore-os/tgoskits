//! Strongly typed interrupt-controller identifiers and shared errors.

/// Highest architected LoongArch CPU interrupt line used by ECFG/ESTAT.
pub const MAX_CPU_IRQ_LINE: usize = 12;
/// Maximum EIOINTC vector count supported by this driver.
pub const MAX_EIO_VECTORS: usize = 256;
/// Maximum PCH-PIC input count supported by this driver.
pub const MAX_PCH_INPUTS: usize = 64;
/// LIOINTC input count.
pub const LIO_INPUT_COUNT: usize = 32;

/// A CPU-local interrupt line reported by `ESTAT.IS`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct CpuIrqLine(u8);

impl CpuIrqLine {
    /// Validates a raw CPU interrupt line.
    ///
    /// # Errors
    ///
    /// Returns [`IntcError::InvalidCpuIrqLine`] when `raw` is outside the
    /// LoongArch ECFG/ESTAT interrupt-line range.
    pub const fn new(raw: usize) -> Result<Self, IntcError> {
        if raw <= MAX_CPU_IRQ_LINE {
            Ok(Self(raw as u8))
        } else {
            Err(IntcError::InvalidCpuIrqLine(raw))
        }
    }

    /// Returns the hardware line number.
    pub const fn raw(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<usize> for CpuIrqLine {
    type Error = IntcError;

    fn try_from(raw: usize) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

/// A controller-local EIOINTC vector.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct EioVector(u16);

impl EioVector {
    /// Validates a raw EIOINTC vector against the hardware maximum.
    ///
    /// A controller configured with fewer vectors performs an additional
    /// per-instance range check when the vector is used.
    pub const fn new(raw: usize) -> Result<Self, IntcError> {
        if raw < MAX_EIO_VECTORS {
            Ok(Self(raw as u16))
        } else {
            Err(IntcError::InvalidEioVector(raw))
        }
    }

    /// Returns the controller-local vector number.
    pub const fn raw(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<usize> for EioVector {
    type Error = IntcError;

    fn try_from(raw: usize) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

/// A controller-local PCH-PIC input.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PchInput(u8);

impl PchInput {
    /// Validates a raw PCH-PIC input against the hardware maximum.
    pub const fn new(raw: usize) -> Result<Self, IntcError> {
        if raw < MAX_PCH_INPUTS {
            Ok(Self(raw as u8))
        } else {
            Err(IntcError::InvalidPchInput(raw))
        }
    }

    /// Returns the controller-local input number.
    pub const fn raw(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<usize> for PchInput {
    type Error = IntcError;

    fn try_from(raw: usize) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

/// A controller-local LIOINTC input.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct LioInput(u8);

impl LioInput {
    /// Validates a raw LIOINTC input.
    pub const fn new(raw: usize) -> Result<Self, IntcError> {
        if raw < LIO_INPUT_COUNT {
            Ok(Self(raw as u8))
        } else {
            Err(IntcError::InvalidLioInput(raw))
        }
    }

    /// Returns the controller-local input number.
    pub const fn raw(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<usize> for LioInput {
    type Error = IntcError;

    fn try_from(raw: usize) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

/// Errors reported by the portable LoongArch interrupt-controller core.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IntcError {
    /// A raw CPU interrupt line is outside the ECFG/ESTAT range.
    #[error("CPU interrupt line {0} is outside the LoongArch ECFG/ESTAT range")]
    InvalidCpuIrqLine(usize),
    /// A raw EIOINTC vector exceeds the hardware maximum.
    #[error("EIOINTC vector {0} is outside the hardware range")]
    InvalidEioVector(usize),
    /// A raw PCH-PIC input exceeds the hardware maximum.
    #[error("PCH-PIC input {0} is outside the hardware range")]
    InvalidPchInput(usize),
    /// A raw LIOINTC input exceeds the hardware maximum.
    #[error("LIOINTC input {0} is outside the hardware range")]
    InvalidLioInput(usize),
    /// A typed identifier is valid for the hardware family but not this
    /// configured controller instance.
    #[error("{kind} index {index} is outside configured count {count}")]
    OutsideConfiguredRange {
        /// Controller-local identifier kind.
        kind: &'static str,
        /// Rejected identifier.
        index: usize,
        /// Configured number of identifiers.
        count: usize,
    },
    /// A controller count is zero, too small, or exceeds the hardware limit.
    #[error("{controller} count {count} is outside supported range {min}..={max}")]
    InvalidCount {
        /// Controller name.
        controller: &'static str,
        /// Rejected count.
        count: usize,
        /// Smallest supported count.
        min: usize,
        /// Largest supported count.
        max: usize,
    },
    /// A count does not align to the controller register grouping.
    #[error("{controller} count {count} is not a multiple of {granularity}")]
    InvalidCountGranularity {
        /// Controller name.
        controller: &'static str,
        /// Rejected count.
        count: usize,
        /// Required register grouping.
        granularity: usize,
    },
    /// A mapped region cannot cover every register touched by the driver.
    #[error("{region} MMIO region is {actual:#x} bytes, requires at least {required:#x}")]
    MmioTooSmall {
        /// Region name.
        region: &'static str,
        /// Supplied byte length.
        actual: usize,
        /// Minimum byte length.
        required: usize,
    },
    /// A mapped region does not satisfy the typed register block alignment.
    #[error("{region} MMIO address {address:#x} is not aligned to {required_alignment} bytes")]
    MmioMisaligned {
        /// Region name.
        region: &'static str,
        /// Supplied virtual address.
        address: usize,
        /// Natural alignment required by the register block.
        required_alignment: usize,
    },
    /// Adding the PCH base vector and input count overflowed or exceeded the
    /// EIO vector namespace.
    #[error("PCH-PIC vector range base={base} count={count} exceeds EIOINTC capacity")]
    InvalidPchVectorRange {
        /// First EIO vector assigned to the PIC.
        base: usize,
        /// Number of PIC inputs.
        count: usize,
    },
    /// LIOINTC has no parent CPU interrupt line.
    #[error("LIOINTC requires at least one parent CPU interrupt line")]
    MissingLioParent,
    /// A LIO parent line does not match its INT0..INT3 route slot.
    #[error("LIOINTC parent slot {slot} requires CPU line {expected}, got {actual}")]
    InvalidLioParentSlot {
        /// INT0..INT3 slot.
        slot: usize,
        /// Required CPU line for that slot.
        expected: usize,
        /// Supplied CPU line.
        actual: usize,
    },
    /// A LIO input map references a parent slot with no CPU line.
    #[error("LIOINTC parent slot {slot} has input map {map:#x} but no CPU line")]
    LioMapWithoutParent {
        /// INT0..INT3 slot.
        slot: usize,
        /// Rejected input bitmap.
        map: u32,
    },
    /// A firmware interrupt specifier contains no cells.
    #[error("{controller} interrupt specifier is empty")]
    EmptySpecifier {
        /// Controller whose specifier was parsed.
        controller: &'static str,
    },
}
