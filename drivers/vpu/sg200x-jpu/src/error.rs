//! Typed SG200x JPU failure domains.

use dma_api::DmaError;
use thiserror::Error;

use super::{engine::PollError, layout::FrameLayoutError};

/// JPEG validation and header parsing failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum JpegHeaderError {
    /// The stream does not begin with a JPEG start-of-image marker.
    #[error("JPEG stream does not start with SOI")]
    MissingSoi,
    /// The parsed entropy-coded stream offset is not inside the input.
    #[error("JPEG ECS offset is out of bounds")]
    EcsOffsetOutOfBounds,
    /// The stream has no entropy byte followed by an end-of-image marker.
    #[error("JPEG stream has no entropy data followed by EOI")]
    MissingEntropyDataAndEoi,
    /// The baseline frame header is too short.
    #[error("SOF too short")]
    SofTooShort,
    /// The JPU accepts only eight-bit baseline samples.
    #[error("only 8-bit baseline JPEG is supported")]
    BaselinePrecisionUnsupported,
    /// The component count is neither grayscale nor three-component color.
    #[error("only grayscale and three-component JPEG are supported")]
    ComponentCountUnsupported,
    /// The baseline frame component length overflowed.
    #[error("SOF component length overflow")]
    SofComponentLengthOverflow,
    /// The baseline frame component payload has an unexpected length.
    #[error("SOF component payload has an invalid length")]
    SofComponentPayloadInvalid,
    /// A baseline frame references a quantization table outside the hardware range.
    #[error("SOF quantization table index is out of range")]
    SofQuantizationTableOutOfRange,
    /// A chroma component uses unsupported sampling factors.
    #[error("unsupported JPEG chroma sampling factors")]
    ChromaSamplingUnsupported,
    /// The luma component uses unsupported sampling factors.
    #[error("unsupported JPEG luma sampling factors")]
    LumaSamplingUnsupported,
    /// A grayscale component uses unsupported sampling factors.
    #[error("unsupported grayscale sampling factors")]
    GrayscaleSamplingUnsupported,
    /// Progressive JPEG decoding is not implemented by this driver.
    #[error("progressive JPEG is unsupported")]
    ProgressiveUnsupported,
    /// The scan header is not followed by entropy-coded data.
    #[error("SOS has no entropy-coded data")]
    SosHasNoEntropyData,
    /// The scan header is too short.
    #[error("SOS too short")]
    SosTooShort,
    /// The scan components do not match the frame components.
    #[error("SOS components do not match SOF")]
    SosComponentsMismatch,
    /// The scan component length overflowed.
    #[error("SOS component length overflow")]
    SosComponentLengthOverflow,
    /// The scan component payload has an unexpected length.
    #[error("SOS component payload has an invalid length")]
    SosComponentPayloadInvalid,
    /// A scan references a Huffman table outside the hardware range.
    #[error("SOS Huffman table index is out of range")]
    SosHuffmanTableOutOfRange,
    /// The scan parameters are not baseline sequential JPEG parameters.
    #[error("non-baseline SOS parameters are unsupported")]
    SosParametersUnsupported,
    /// The restart interval segment has an unexpected length.
    #[error("DRI has an invalid length")]
    DriLengthInvalid,
    /// No scan header was found.
    #[error("SOS not found")]
    SosNotFound,
    /// A marker offset overflowed the host address type.
    #[error("JPEG marker offset overflow")]
    MarkerOffsetOverflow,
    /// A scan payload extends beyond the input stream.
    #[error("SOS payload exceeds JPEG stream")]
    SosPayloadExceedsStream,
    /// A marker does not contain its complete length field.
    #[error("JPEG marker length is truncated")]
    MarkerLengthTruncated,
    /// A marker length is smaller than the JPEG length field itself.
    #[error("JPEG marker length is invalid")]
    MarkerLengthInvalid,
    /// A marker end offset overflowed the host address type.
    #[error("JPEG marker length overflow")]
    MarkerLengthOverflow,
    /// A marker payload extends beyond the input stream.
    #[error("JPEG marker payload exceeds stream")]
    MarkerPayloadExceedsStream,
    /// A Huffman table payload extends beyond the input stream.
    #[error("DHT payload exceeds JPEG stream")]
    DhtPayloadExceedsStream,
    /// A Huffman table length overflowed the host address type.
    #[error("DHT table length overflow")]
    DhtTableLengthOverflow,
    /// A Huffman table does not contain all code-length counts.
    #[error("DHT table counts are truncated")]
    DhtCountsTruncated,
    /// A Huffman table class or index is outside the hardware range.
    #[error("DHT table class or index is out of range")]
    DhtClassOrIndexOutOfRange,
    /// A DC Huffman code length exceeds the hardware table.
    #[error("DC Huffman code length exceeds the hardware table")]
    DcHuffmanCodeTooLong,
    /// A Huffman table has more symbols than the baseline hardware supports.
    #[error("DHT symbol count exceeds the baseline hardware table")]
    DhtSymbolCountTooLarge,
    /// A Huffman table contains more than 256 values.
    #[error("DHT defines more than 256 values")]
    DhtTooManyValues,
    /// A Huffman value range overflowed the host address type.
    #[error("DHT values length overflow")]
    DhtValuesLengthOverflow,
    /// A Huffman table does not contain all declared values.
    #[error("DHT values are truncated")]
    DhtValuesTruncated,
    /// A quantization table payload extends beyond the input stream.
    #[error("DQT payload exceeds JPEG stream")]
    DqtPayloadExceedsStream,
    /// A quantization table index is outside the hardware range.
    #[error("DQT table index is out of range")]
    DqtTableIndexOutOfRange,
    /// A quantization table uses an unsupported precision.
    #[error("DQT precision is unsupported")]
    DqtPrecisionUnsupported,
    /// A quantization value range overflowed the host address type.
    #[error("DQT values length overflow")]
    DqtValuesLengthOverflow,
    /// A quantization table length overflowed the host address type.
    #[error("DQT table length overflow")]
    DqtTableLengthOverflow,
    /// A quantization table does not contain all declared values.
    #[error("DQT values are truncated")]
    DqtValuesTruncated,
    /// The entropy-coded data offset is at or beyond the stream end.
    #[error("JPEG entropy-coded data starts outside the stream")]
    EntropyDataOutsideStream,
    /// The mandatory GRAM prefetch range overflowed the host address type.
    #[error("JPU GRAM prefetch range overflow")]
    GramPrefetchRangeOverflow,
}

/// Failures while representing DMA buffers in the JPU's 32-bit registers.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum JpuDmaAddressError {
    /// The used DMA length is zero or exceeds its allocation.
    #[error("invalid JPU DMA region length")]
    InvalidRegionLength,
    /// The DMA region end address overflowed.
    #[error("JPU DMA address range overflow")]
    AddressRangeOverflow,
    /// The DMA start address does not fit a JPU register.
    #[error("JPU DMA start does not fit u32")]
    StartDoesNotFitU32,
    /// The DMA end address does not fit a JPU register.
    #[error("JPU DMA end does not fit u32")]
    EndDoesNotFitU32,
    /// A DMA offset does not fit a JPU register.
    #[error("JPU DMA offset does not fit u32")]
    OffsetDoesNotFitU32,
    /// Adding an offset to a DMA start address overflowed.
    #[error("JPU DMA offset address overflow")]
    OffsetAddressOverflow,
    /// A DMA offset points outside its registered region.
    #[error("JPU DMA offset is outside its region")]
    OffsetOutsideRegion,
    /// Adding a frame-plane offset overflowed.
    #[error("JPU frame plane address overflow")]
    FramePlaneAddressOverflow,
    /// A frame-plane address does not fit a JPU register.
    #[error("JPU frame plane does not fit u32")]
    FramePlaneDoesNotFitU32,
    /// A frame plane starts outside its registered DMA region.
    #[error("JPU frame plane starts outside DMA region")]
    FramePlaneOutsideRegion,
    /// A DMA allocation length does not fit the transport address type.
    #[error("JPU DMA allocation length does not fit u64")]
    AllocationLengthDoesNotFitU64,
    /// A DMA allocation exceeds the JPU's 32-bit address window.
    #[error("JPU DMA allocation exceeds the 32-bit address window")]
    AllocationExceedsAddressWindow,
}

/// Internal frame and stream buffer invariant failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum JpuBufferError {
    /// A completed decode has no frame buffer.
    #[error("missing completed frame buffer")]
    MissingCompletedFrameBuffer,
    /// The requested frame view exceeds the DMA allocation.
    #[error("frame view exceeds its DMA allocation")]
    FrameViewExceedsAllocation,
    /// A requested frame is not the most recently completed frame.
    #[error("requested frame does not match the most recent decode")]
    CompletedFrameMismatch,
    /// A zero-sized stream DMA buffer was requested.
    #[error("zero-sized stream buffer")]
    ZeroSizedStreamBuffer,
    /// A zero-sized frame DMA buffer was requested.
    #[error("zero-sized frame buffer")]
    ZeroSizedFrameBuffer,
    /// Decoder state does not contain its stream buffer.
    #[error("missing stream buffer")]
    MissingStreamBuffer,
    /// Decoder state does not contain its frame buffer.
    #[error("missing frame buffer")]
    MissingFrameBuffer,
    /// Input bytes exceed the allocated stream DMA buffer.
    #[error("stream data exceeds its DMA allocation")]
    StreamDataExceedsAllocation,
    /// The calculated frame layout exceeds the frame DMA buffer.
    #[error("JPU frame layout exceeds its DMA allocation")]
    FrameLayoutExceedsAllocation,
    /// The calculated frame planes exceed the logical frame length.
    #[error("JPU frame planes exceed total length")]
    FramePlanesExceedTotalLength,
    /// A requested logical frame exceeds the DMA allocation.
    #[error("logical frame exceeds its DMA allocation")]
    LogicalFrameExceedsAllocation,
    /// A frame-plane end offset overflowed.
    #[error("JPU plane end overflow")]
    PlaneEndOverflow,
    /// Frame planes overlap or exceed the logical frame length.
    #[error("JPU frame planes overlap or exceed total length")]
    FramePlanesOverlapOrExceedTotalLength,
    /// A frame-plane stride does not fit the host address type.
    #[error("JPU plane stride overflow")]
    PlaneStrideOverflow,
    /// A frame-plane width does not fit the host address type.
    #[error("JPU plane width overflow")]
    PlaneWidthOverflow,
    /// A frame-plane height does not fit the host address type.
    #[error("JPU plane height overflow")]
    PlaneHeightOverflow,
    /// A frame-plane row is wider than its stride.
    #[error("JPU plane width exceeds its stride")]
    PlaneWidthExceedsStride,
    /// A row-padding start offset overflowed.
    #[error("JPU plane row padding offset overflow")]
    RowPaddingOffsetOverflow,
    /// A row-padding end offset overflowed.
    #[error("JPU plane row padding end overflow")]
    RowPaddingEndOverflow,
    /// Row padding extends beyond the frame buffer.
    #[error("JPU plane row padding exceeds frame buffer")]
    RowPaddingExceedsFrameBuffer,
}

/// Failures while polling JPU registers for a required state transition.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum JpuRegisterError {
    /// The software reset bit did not clear before the poll limit.
    #[error("JPU software reset timed out")]
    SoftwareResetTimeout,
    /// The bitstream-buffer controller did not become idle before the poll limit.
    #[error("JPU BBC did not become idle")]
    BbcIdleTimeout,
}

/// Failures while preparing the JPU's internal GRAM window.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum JpuHardwareSetupError {
    /// A required register state transition failed.
    #[error(transparent)]
    Register(#[from] JpuRegisterError),
    /// The current GRAM page index overflowed.
    #[error("JPU GRAM page index overflow")]
    PageIndexOverflow,
    /// The current GRAM page offset overflowed.
    #[error("JPU GRAM page offset overflow")]
    PageOffsetOverflow,
    /// The current GRAM page index does not fit a JPU register.
    #[error("JPU GRAM page index does not fit u32")]
    PageIndexDoesNotFitU32,
    /// The next GRAM page index overflowed.
    #[error("JPU GRAM next page index overflow")]
    NextPageIndexOverflow,
    /// The next GRAM page index does not fit a JPU register.
    #[error("JPU GRAM next page does not fit u32")]
    NextPageDoesNotFitU32,
    /// A stream address used during GRAM setup is invalid.
    #[error(transparent)]
    DmaAddress(#[from] JpuDmaAddressError),
}

/// Error returned while acquiring the singleton JPU engine.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum JpuCreateError {
    /// Another live decoder already owns the hardware engine.
    #[error("SG200x JPU is already owned by another decoder")]
    AlreadyOwned,
    /// Clock/reset initialization did not reach its ready state.
    #[error("SG200x JPU initialization failed: {0}")]
    Initialization(#[from] JpuRegisterError),
}

/// Error returned while inspecting a JPEG without accessing JPU hardware.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum JpuInspectError {
    /// The compressed input is empty.
    #[error("JPEG stream is empty")]
    EmptyStream,
    /// JPEG marker or table parsing failed.
    #[error("invalid JPEG stream: {0}")]
    InvalidJpeg(#[from] JpegHeaderError),
    /// The requested scale or planar layout is unsupported.
    #[error("invalid JPU frame layout: {0}")]
    Layout(#[from] FrameLayoutError),
}

/// Error returned by JPEG decode operations.
#[derive(Debug, Error)]
pub enum JpuDecodeError {
    /// A previous decode failure or partial DMA setup left hardware ownership unknown.
    #[error("SG200x JPU is poisoned after an incomplete DMA operation; reboot is required")]
    Poisoned,
    /// The compressed input is empty.
    #[error("JPEG stream is empty")]
    EmptyStream,
    /// JPEG marker or table parsing failed.
    #[error("invalid JPEG stream: {0}")]
    InvalidJpeg(#[from] JpegHeaderError),
    /// The requested scale or planar layout is unsupported.
    #[error("invalid JPU frame layout: {0}")]
    Layout(#[from] FrameLayoutError),
    /// A stream or frame DMA allocation failed.
    #[error("JPU DMA allocation failed: {0}")]
    Dma(#[from] DmaError),
    /// An internal buffer length invariant was violated.
    #[error("invalid JPU buffer state: {0}")]
    BufferInvariant(#[from] JpuBufferError),
    /// A DMA address cannot be represented by the 32-bit JPU registers.
    #[error("invalid JPU DMA address: {0}")]
    DmaAddress(#[from] JpuDmaAddressError),
    /// Register or GRAM setup failed after DMA ownership was prepared.
    #[error("JPU hardware setup failed: {0}")]
    HardwareSetup(#[from] JpuHardwareSetupError),
    /// The JPU reported an error before DMA quiescence could be proven.
    #[error(
        "SG200x JPU reported a decode error before DMA quiescence was proven; reboot is required"
    )]
    DecodeFailed,
    /// The JPU did not reach a terminal state before the poll limit.
    #[error("SG200x JPU decode timed out; reboot is required")]
    Timeout,
}

impl From<JpuInspectError> for JpuDecodeError {
    fn from(error: JpuInspectError) -> Self {
        match error {
            JpuInspectError::EmptyStream => Self::EmptyStream,
            JpuInspectError::InvalidJpeg(error) => Self::InvalidJpeg(error),
            JpuInspectError::Layout(error) => Self::Layout(error),
        }
    }
}

impl From<PollError> for JpuDecodeError {
    fn from(error: PollError) -> Self {
        match error {
            PollError::Decode => Self::DecodeFailed,
            PollError::Timeout => Self::Timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use core::error::Error as _;

    use super::{
        JpegHeaderError, JpuDecodeError, JpuHardwareSetupError, JpuInspectError, JpuRegisterError,
    };

    #[test]
    fn nested_errors_preserve_display_text_and_sources() {
        let inspect = JpuInspectError::from(JpegHeaderError::ProgressiveUnsupported);
        assert_eq!(
            inspect.to_string(),
            "invalid JPEG stream: progressive JPEG is unsupported"
        );
        assert!(inspect.source().is_some());

        let setup = JpuHardwareSetupError::from(JpuRegisterError::BbcIdleTimeout);
        let decode = JpuDecodeError::from(setup);
        assert_eq!(
            decode.to_string(),
            "JPU hardware setup failed: JPU BBC did not become idle"
        );
        assert!(decode.source().and_then(|error| error.source()).is_some());
    }
}
