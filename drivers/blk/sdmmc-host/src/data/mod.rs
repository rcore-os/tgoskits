use alloc::boxed::Box;
use core::{
    fmt,
    num::{NonZeroU16, NonZeroU32},
};

use dma_api::{DmaDirection, PreparedDma};

use crate::bus::Error;

/// Direction of a data phase on DAT lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataDirection {
    Read,
    Write,
}

/// Caller-owned data buffer tied to an in-flight transaction lifetime.
pub enum DataBuffer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
    Dma(PreparedDma),
}

impl DataBuffer<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::Read(buf) => buf.len(),
            Self::Write(buf) => buf.len(),
            Self::Dma(buffer) => buffer.len().get(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn matches_direction(&self, direction: DataDirection) -> bool {
        match self {
            Self::Read(_) => direction == DataDirection::Read,
            Self::Write(_) => direction == DataDirection::Write,
            Self::Dma(buffer) => matches!(
                (buffer.direction(), direction),
                (DmaDirection::FromDevice, DataDirection::Read)
                    | (DmaDirection::ToDevice, DataDirection::Write)
                    | (DmaDirection::Bidirectional, _)
            ),
        }
    }
}

pub type DataTransfer<'a> = DataBuffer<'a>;

/// Error returned while constructing an owned-DMA data phase.
pub struct DmaPhaseError {
    error: Error,
    buffer: Box<PreparedDma>,
}

impl DmaPhaseError {
    fn new(error: Error, buffer: PreparedDma) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub const fn error(&self) -> Error {
        self.error
    }

    pub fn into_buffer(self) -> PreparedDma {
        *self.buffer
    }

    pub fn into_parts(self) -> (Error, PreparedDma) {
        (self.error, *self.buffer)
    }
}

impl fmt::Debug for DmaPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaPhaseError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for DmaPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl core::error::Error for DmaPhaseError {}

/// Optional data phase associated with a command.
pub struct DataPhase<'a> {
    pub direction: DataDirection,
    pub block_size: NonZeroU16,
    pub block_count: NonZeroU32,
    pub buffer: DataBuffer<'a>,
}

impl<'a> DataPhase<'a> {
    pub fn read(
        block_size: NonZeroU16,
        block_count: NonZeroU32,
        buffer: &'a mut [u8],
    ) -> Result<Self, Error> {
        let phase = Self {
            direction: DataDirection::Read,
            block_size,
            block_count,
            buffer: DataBuffer::Read(buffer),
        };
        phase.validate()?;
        Ok(phase)
    }

    pub fn write(
        block_size: NonZeroU16,
        block_count: NonZeroU32,
        buffer: &'a [u8],
    ) -> Result<Self, Error> {
        let phase = Self {
            direction: DataDirection::Write,
            block_size,
            block_count,
            buffer: DataBuffer::Write(buffer),
        };
        phase.validate()?;
        Ok(phase)
    }

    pub fn dma(
        direction: DataDirection,
        block_size: NonZeroU16,
        block_count: NonZeroU32,
        buffer: PreparedDma,
    ) -> Result<Self, DmaPhaseError> {
        let phase = Self {
            direction,
            block_size,
            block_count,
            buffer: DataBuffer::Dma(buffer),
        };
        match phase.validate() {
            Ok(()) => Ok(phase),
            Err(err) => {
                let DataBuffer::Dma(buffer) = phase.buffer else {
                    unreachable!("DataPhase::dma always stores a DMA buffer")
                };
                Err(DmaPhaseError::new(err, buffer))
            }
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        let expected = usize::from(self.block_size.get())
            .checked_mul(
                usize::try_from(self.block_count.get()).map_err(|_| Error::InvalidArgument)?,
            )
            .ok_or(Error::InvalidArgument)?;
        if self.buffer.len() != expected {
            return Err(Error::InvalidArgument);
        }
        if !self.buffer.matches_direction(self.direction) {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }
}
