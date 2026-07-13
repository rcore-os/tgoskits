//! Synchronous SG200x JPU decode orchestration and DMA ownership.

use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::{CpuDmaBuffer, DeviceDma, DmaDirection, DmaError, InFlightDma};
use thiserror::Error;

use super::{
    engine::{
        BBC_STREAM_PAGE_SIZE, GRAM_PREFETCH_PAGES, HardwareDecodeInfo, PollError,
        checked_dma_offset, checked_dma_region, checked_frame_dma_addresses, configure_stream_regs,
        gram_setup, poll_decode_done, start_decode, upload_huff_tables, upload_quant_tables,
    },
    header::parse_jpeg_header,
    layout::{FrameLayout, FrameLayoutError, JpuPixelFormat, JpuScale, PlaneLayout},
    regs::hardware_init_at,
};

const DMA_ALIGNMENT: usize = 16 * 1024;

static JPU_IN_USE: AtomicBool = AtomicBool::new(false);

/// Caller-mapped MMIO bases required by the SG200x JPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JpuMmio {
    /// JPU register block.
    pub jpu_base: usize,
    /// TOP clock/reset register block.
    pub top_base: usize,
    /// Video-codec control register block.
    pub vc_base: usize,
}

impl JpuMmio {
    /// Creates one set of caller-mapped JPU register bases.
    pub const fn new(jpu_base: usize, top_base: usize, vc_base: usize) -> Self {
        Self {
            jpu_base,
            top_base,
            vc_base,
        }
    }
}

/// Error returned while acquiring the singleton JPU engine.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum JpuCreateError {
    /// Another live decoder already owns the hardware engine.
    #[error("SG200x JPU is already owned by another decoder")]
    AlreadyOwned,
    /// Clock/reset initialization did not reach its ready state.
    #[error("SG200x JPU initialization failed: {0}")]
    Initialization(&'static str),
}

/// Error returned by JPEG decode operations.
#[derive(Debug, Error)]
pub enum JpuDecodeError {
    /// A previous timeout or partial DMA setup left hardware ownership unknown.
    #[error("SG200x JPU is poisoned after an incomplete DMA operation; reboot is required")]
    Poisoned,
    /// The compressed input is empty.
    #[error("JPEG stream is empty")]
    EmptyStream,
    /// JPEG marker or table parsing failed.
    #[error("invalid JPEG stream: {0}")]
    InvalidJpeg(&'static str),
    /// The requested scale or planar layout is unsupported.
    #[error("invalid JPU frame layout: {0}")]
    Layout(#[from] FrameLayoutError),
    /// A stream or frame DMA allocation failed.
    #[error("JPU DMA allocation failed: {0}")]
    Dma(#[from] DmaError),
    /// An internal buffer length invariant was violated.
    #[error("invalid JPU buffer state: {0}")]
    BufferInvariant(&'static str),
    /// A DMA address cannot be represented by the 32-bit JPU registers.
    #[error("invalid JPU DMA address: {0}")]
    DmaAddress(&'static str),
    /// Register or GRAM setup failed after DMA ownership was prepared.
    #[error("JPU hardware setup failed: {0}")]
    HardwareSetup(&'static str),
    /// The JPU reported a terminal decode error.
    #[error("SG200x JPU reported a decode error")]
    DecodeFailed,
    /// The JPU did not reach a terminal state before the poll limit.
    #[error("SG200x JPU decode timed out; reboot is required")]
    Timeout,
}

/// A borrowed planar frame produced by the JPU.
///
/// The borrow prevents the decoder from starting another operation while the
/// returned DMA buffer is still in use.
///
/// ```compile_fail
/// use sg200x_jpu::JpuDecoder;
///
/// fn cannot_decode_twice(decoder: &mut JpuDecoder, jpeg: &[u8]) {
///     let first = decoder.decode(jpeg).unwrap();
///     let _second = decoder.decode(jpeg).unwrap();
///     let _still_borrowed = first.yuv_data;
/// }
/// ```
#[non_exhaustive]
#[derive(Debug)]
pub struct DecodeResult<'a> {
    /// Meaningful output width after scaling, excluding coded padding.
    pub width: u32,
    /// Meaningful output height after scaling, excluding coded padding.
    pub height: u32,
    /// CPU-visible frame bytes. Plane offsets and strides are in [`Self::layout`].
    pub yuv_data: &'a [u8],
    /// Device-visible address corresponding to `yuv_data[0]`.
    pub yuv_dma_addr: u32,
    /// Planar format, scale, extents, offsets, and strides.
    pub layout: FrameLayout,
}

/// Singleton synchronous SG200x JPU decoder.
pub struct JpuDecoder {
    mmio: JpuMmio,
    dma: DeviceDma,
    stream_buffer: Option<CpuDmaBuffer>,
    frame_buffer: Option<CpuDmaBuffer>,
    poisoned: bool,
}

impl JpuDecoder {
    /// Acquires and initializes the SG200x JPU.
    ///
    /// # Safety
    ///
    /// Every base in `mmio` must be four-byte aligned and remain a valid device
    /// mapping for the decoder's lifetime. The mappings must cover at least the
    /// JPU register range through offset `0x237`, the TOP range through offset
    /// `0x3003`, and four bytes at the VC base. No code outside this decoder may
    /// access or reconfigure the JPU while it is owned. `dma` must allocate
    /// memory visible to this JPU and implement the cache-maintenance contract
    /// required by `dma-api`.
    pub unsafe fn new(mmio: JpuMmio, dma: DeviceDma) -> Result<Self, JpuCreateError> {
        JPU_IN_USE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| JpuCreateError::AlreadyOwned)?;

        if let Err(error) = hardware_init_at(mmio.jpu_base, mmio.top_base, mmio.vc_base) {
            JPU_IN_USE.store(false, Ordering::Release);
            return Err(JpuCreateError::Initialization(error));
        }
        Ok(Self {
            mmio,
            dma,
            stream_buffer: None,
            frame_buffer: None,
            poisoned: false,
        })
    }

    /// Decodes at the full coded resolution.
    pub fn decode<'a>(&'a mut self, jpeg_data: &[u8]) -> Result<DecodeResult<'a>, JpuDecodeError> {
        self.decode_scaled(jpeg_data, JpuScale::Full)
    }

    /// Decodes a baseline JPEG using one isotropic hardware downscale mode.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid JPEG data, unsupported layouts, DMA
    /// allocation/address failures, terminal hardware errors, and timeouts. A
    /// timeout poisons the decoder because the device may still own its DMA
    /// buffers; subsequent calls return [`JpuDecodeError::Poisoned`].
    pub fn decode_scaled<'a>(
        &'a mut self,
        jpeg_data: &[u8],
        scale: JpuScale,
    ) -> Result<DecodeResult<'a>, JpuDecodeError> {
        self.validate_decode_request(jpeg_data)?;

        let header = parse_jpeg_header(jpeg_data).map_err(JpuDecodeError::InvalidJpeg)?;
        let format = JpuPixelFormat::from_raw(header.format)?;
        let layout = FrameLayout::new(header.width, header.height, format, scale)?;
        let stream_len = required_stream_capacity(jpeg_data.len(), header.ecs_offset)
            .map_err(JpuDecodeError::InvalidJpeg)?;
        validate_dma_allocation_len(stream_len).map_err(JpuDecodeError::DmaAddress)?;
        validate_dma_allocation_len(layout.total_len).map_err(JpuDecodeError::DmaAddress)?;
        self.ensure_buffers(stream_len, layout.total_len)?;
        self.write_stream(jpeg_data, stream_len)?;

        let stream_dma = self.stream_dma_region(stream_len)?;
        let frame_dma = self.frame_dma_region(layout.total_len)?;
        let stream_data_end = checked_dma_offset(stream_dma, jpeg_data.len(), true)
            .map_err(JpuDecodeError::DmaAddress)?;
        let frame_planes =
            checked_frame_dma_addresses(frame_dma, &layout).map_err(JpuDecodeError::DmaAddress)?;
        let hardware = HardwareDecodeInfo::for_format(format);

        configure_stream_regs(
            self.mmio.jpu_base,
            stream_dma,
            stream_data_end,
            jpeg_data.len(),
            &header,
            &layout,
            hardware,
        );
        upload_huff_tables(self.mmio.jpu_base, &header).map_err(JpuDecodeError::HardwareSetup)?;
        upload_quant_tables(self.mmio.jpu_base, &header).map_err(JpuDecodeError::HardwareSetup)?;

        let (stream_in_flight, frame_in_flight) = self.begin_dma()?;
        if let Err(error) = gram_setup(self.mmio.jpu_base, stream_dma, &header) {
            self.quarantine_after_incomplete_dma(stream_in_flight, frame_in_flight);
            return Err(JpuDecodeError::HardwareSetup(error));
        }
        start_decode(self.mmio.jpu_base, frame_planes, &header, &layout);

        match poll_decode_done(self.mmio.jpu_base) {
            Ok(()) => {
                self.complete_dma(stream_in_flight, frame_in_flight);
            }
            Err(PollError::Decode) => {
                self.complete_dma(stream_in_flight, frame_in_flight);
                return Err(JpuDecodeError::DecodeFailed);
            }
            Err(PollError::Timeout) => {
                self.quarantine_after_incomplete_dma(stream_in_flight, frame_in_flight);
                return Err(JpuDecodeError::Timeout);
            }
        }

        self.clear_frame_padding(&layout)?;
        let frame = self
            .frame_buffer
            .as_ref()
            .ok_or(JpuDecodeError::BufferInvariant(
                "missing completed frame buffer",
            ))?;
        let yuv_data =
            frame
                .as_slice_cpu()
                .get(..layout.total_len)
                .ok_or(JpuDecodeError::BufferInvariant(
                    "frame view exceeds its DMA allocation",
                ))?;

        Ok(DecodeResult {
            width: layout.visible.width,
            height: layout.visible.height,
            yuv_data,
            yuv_dma_addr: frame_dma.start,
            layout,
        })
    }

    fn validate_decode_request(&self, jpeg_data: &[u8]) -> Result<(), JpuDecodeError> {
        if self.poisoned {
            return Err(JpuDecodeError::Poisoned);
        }
        if jpeg_data.is_empty() {
            return Err(JpuDecodeError::EmptyStream);
        }
        Ok(())
    }

    fn ensure_buffers(
        &mut self,
        stream_len: usize,
        frame_len: usize,
    ) -> Result<(), JpuDecodeError> {
        let stream_capacity = self
            .stream_buffer
            .as_ref()
            .map_or(0, |buffer| buffer.len().get());
        let frame_capacity = self
            .frame_buffer
            .as_ref()
            .map_or(0, |buffer| buffer.len().get());
        if buffer_plan(stream_capacity, frame_capacity, stream_len, frame_len) == BufferPlan::Reuse
        {
            return Ok(());
        }

        let stream_len = NonZeroUsize::new(stream_len)
            .ok_or(JpuDecodeError::BufferInvariant("zero-sized stream buffer"))?;
        let frame_len = NonZeroUsize::new(frame_len)
            .ok_or(JpuDecodeError::BufferInvariant("zero-sized frame buffer"))?;
        let stream =
            CpuDmaBuffer::new_zero(&self.dma, stream_len, DMA_ALIGNMENT, DmaDirection::ToDevice)?;
        let frame = CpuDmaBuffer::new_zero(
            &self.dma,
            frame_len,
            DMA_ALIGNMENT,
            DmaDirection::FromDevice,
        )?;
        self.stream_buffer = Some(stream);
        self.frame_buffer = Some(frame);
        Ok(())
    }

    fn write_stream(&mut self, jpeg_data: &[u8], stream_len: usize) -> Result<(), JpuDecodeError> {
        let stream = self
            .stream_buffer
            .as_mut()
            .ok_or(JpuDecodeError::BufferInvariant("missing stream buffer"))?;
        if stream_len > stream.len().get() || jpeg_data.len() > stream_len {
            return Err(JpuDecodeError::BufferInvariant(
                "stream data exceeds its DMA allocation",
            ));
        }

        // SAFETY: the buffer is CPU-owned until begin_dma consumes it. The
        // checked range stays inside this allocation.
        let bytes = unsafe { stream.as_mut_slice_cpu() };
        bytes[..jpeg_data.len()].copy_from_slice(jpeg_data);
        bytes[jpeg_data.len()..stream_len].fill(0);
        Ok(())
    }

    fn stream_dma_region(
        &self,
        stream_len: usize,
    ) -> Result<super::engine::DmaRegion, JpuDecodeError> {
        let stream = self
            .stream_buffer
            .as_ref()
            .ok_or(JpuDecodeError::BufferInvariant("missing stream buffer"))?;
        checked_dma_region(stream.dma_addr().as_u64(), stream.len().get(), stream_len)
            .map_err(JpuDecodeError::DmaAddress)
    }

    fn frame_dma_region(
        &self,
        frame_len: usize,
    ) -> Result<super::engine::DmaRegion, JpuDecodeError> {
        let frame = self
            .frame_buffer
            .as_ref()
            .ok_or(JpuDecodeError::BufferInvariant("missing frame buffer"))?;
        checked_dma_region(frame.dma_addr().as_u64(), frame.len().get(), frame_len)
            .map_err(JpuDecodeError::DmaAddress)
    }

    fn begin_dma(&mut self) -> Result<(InFlightDma, InFlightDma), JpuDecodeError> {
        let stream = self
            .stream_buffer
            .take()
            .ok_or(JpuDecodeError::BufferInvariant("missing stream buffer"))?;
        let frame = match self.frame_buffer.take() {
            Some(frame) => frame,
            None => {
                self.stream_buffer = Some(stream);
                return Err(JpuDecodeError::BufferInvariant("missing frame buffer"));
            }
        };

        let stream = stream.prepare_for_device();
        let frame = frame.prepare_for_device();
        // SAFETY: this state transition occurs immediately before the first
        // register operation that may start stream DMA. Completion is handled
        // only after a terminal status; incomplete operations are quarantined.
        Ok(unsafe { (stream.into_in_flight(), frame.into_in_flight()) })
    }

    fn complete_dma(&mut self, stream: InFlightDma, frame: InFlightDma) {
        // SAFETY: callers invoke this only after the JPU reported a terminal
        // DONE or ERROR status and poll_decode_done acknowledged that status.
        let stream = unsafe { stream.complete_after_quiesce() }.into_cpu_buffer();
        // SAFETY: the same terminal status covers the output frame DMA engine.
        let frame = unsafe { frame.complete_after_quiesce() }.into_cpu_buffer();
        self.stream_buffer = Some(stream);
        self.frame_buffer = Some(frame);
    }

    fn quarantine_after_incomplete_dma(&mut self, stream: InFlightDma, frame: InFlightDma) {
        self.poisoned = true;
        let _stream = stream.quarantine();
        let _frame = frame.quarantine();
    }

    fn clear_frame_padding(&mut self, layout: &FrameLayout) -> Result<(), JpuDecodeError> {
        let frame = self
            .frame_buffer
            .as_mut()
            .ok_or(JpuDecodeError::BufferInvariant(
                "missing completed frame buffer",
            ))?;
        // SAFETY: poll completion returned ownership to the CPU, and the
        // returned mutable slice does not outlive this exclusive decoder borrow.
        let bytes = unsafe { frame.as_mut_slice_cpu() };
        clear_frame_padding(bytes, layout).map_err(JpuDecodeError::BufferInvariant)
    }
}

impl Drop for JpuDecoder {
    fn drop(&mut self) {
        if !self.poisoned {
            JPU_IN_USE.store(false, Ordering::Release);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BufferPlan {
    Reuse,
    ReplaceBoth,
}

const fn buffer_plan(
    stream_capacity: usize,
    frame_capacity: usize,
    stream_len: usize,
    frame_len: usize,
) -> BufferPlan {
    if stream_capacity >= stream_len && frame_capacity >= frame_len {
        BufferPlan::Reuse
    } else {
        BufferPlan::ReplaceBoth
    }
}

fn required_stream_capacity(jpeg_len: usize, ecs_offset: usize) -> Result<usize, &'static str> {
    if ecs_offset >= jpeg_len {
        return Err("JPEG entropy-coded data starts outside the stream");
    }
    let prefetch_start = ecs_offset & !(BBC_STREAM_PAGE_SIZE - 1);
    let prefetch_len = BBC_STREAM_PAGE_SIZE * GRAM_PREFETCH_PAGES;
    let prefetch_end = prefetch_start
        .checked_add(prefetch_len)
        .ok_or("JPU GRAM prefetch range overflow")?;
    Ok(jpeg_len.max(prefetch_end))
}

fn validate_dma_allocation_len(len: usize) -> Result<(), &'static str> {
    let len = u64::try_from(len).map_err(|_| "JPU DMA allocation length does not fit u64")?;
    if len > u32::MAX as u64 {
        return Err("JPU DMA allocation exceeds the 32-bit address window");
    }
    Ok(())
}

fn clear_frame_padding(buffer: &mut [u8], layout: &FrameLayout) -> Result<(), &'static str> {
    if buffer.len() < layout.total_len {
        return Err("JPU frame layout exceeds its DMA allocation");
    }
    let buffer = &mut buffer[..layout.total_len];
    let mut previous_end = 0usize;
    clear_plane_and_gap_padding(buffer, layout.y, &mut previous_end)?;
    if let Some(cb) = layout.cb {
        clear_plane_and_gap_padding(buffer, cb, &mut previous_end)?;
    }
    if let Some(cr) = layout.cr {
        clear_plane_and_gap_padding(buffer, cr, &mut previous_end)?;
    }
    buffer
        .get_mut(previous_end..)
        .ok_or("JPU frame planes exceed total length")?
        .fill(0);
    Ok(())
}

fn clear_plane_and_gap_padding(
    buffer: &mut [u8],
    plane: PlaneLayout,
    previous_end: &mut usize,
) -> Result<(), &'static str> {
    let plane_end = plane
        .offset
        .checked_add(plane.len)
        .ok_or("JPU plane end overflow")?;
    if plane.offset < *previous_end || plane_end > buffer.len() {
        return Err("JPU frame planes overlap or exceed total length");
    }
    buffer[*previous_end..plane.offset].fill(0);

    let stride = usize::try_from(plane.stride).map_err(|_| "JPU plane stride overflow")?;
    let row_bytes = usize::try_from(plane.storage.width).map_err(|_| "JPU plane width overflow")?;
    let rows = usize::try_from(plane.storage.height).map_err(|_| "JPU plane height overflow")?;
    let row_padding = stride
        .checked_sub(row_bytes)
        .ok_or("JPU plane width exceeds its stride")?;
    for row in 0..rows {
        let padding_start = row
            .checked_mul(stride)
            .and_then(|offset| offset.checked_add(plane.offset))
            .and_then(|offset| offset.checked_add(row_bytes))
            .ok_or("JPU plane row padding offset overflow")?;
        let padding_end = padding_start
            .checked_add(row_padding)
            .ok_or("JPU plane row padding end overflow")?;
        buffer
            .get_mut(padding_start..padding_end)
            .ok_or("JPU plane row padding exceeds frame buffer")?
            .fill(0);
    }

    *previous_end = plane_end;
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{
        BufferPlan, buffer_plan, clear_frame_padding, required_stream_capacity,
        validate_dma_allocation_len,
    };
    use crate::{FrameLayout, JpuPixelFormat, JpuScale};

    #[test]
    fn buffer_plan_reuses_only_when_both_capacities_fit() {
        assert_eq!(buffer_plan(128, 1024, 127, 1024), BufferPlan::Reuse);
        assert_eq!(buffer_plan(128, 1024, 129, 100), BufferPlan::ReplaceBoth);
        assert_eq!(buffer_plan(128, 1024, 100, 1025), BufferPlan::ReplaceBoth);
    }

    #[test]
    fn dma_allocation_length_must_fit_the_32_bit_jpu_window() {
        assert!(validate_dma_allocation_len(u32::MAX as usize).is_ok());
        #[cfg(target_pointer_width = "64")]
        assert!(validate_dma_allocation_len(u32::MAX as usize + 1).is_err());
    }

    #[test]
    fn stream_capacity_covers_two_page_gram_prefetch() {
        assert_eq!(required_stream_capacity(1000, 500), Ok(1000));
        assert_eq!(required_stream_capacity(1000, 900), Ok(1280));
        assert_eq!(required_stream_capacity(16_384, 16_383), Ok(16_640));
        assert!(required_stream_capacity(1000, 1000).is_err());
        assert!(required_stream_capacity(1000, 1001).is_err());
        assert!(required_stream_capacity(usize::MAX, usize::MAX - 1).is_err());
    }

    #[test]
    fn frame_padding_is_cleared_without_touching_plane_samples() {
        let layout = FrameLayout::new(129, 129, JpuPixelFormat::Yuv420, JpuScale::Eighth)
            .expect("valid layout");
        let mut memory = std::vec![0xa5u8; layout.total_len];

        clear_frame_padding(&mut memory, &layout).expect("padding layout is valid");

        for row in 0..18 {
            let start = row * 32;
            assert!(memory[start..start + 18].iter().all(|byte| *byte == 0xa5));
            assert!(memory[start + 18..start + 32].iter().all(|byte| *byte == 0));
        }
        for plane_offset in [576, 720] {
            for row in 0..9 {
                let start = plane_offset + row * 16;
                assert!(memory[start..start + 9].iter().all(|byte| *byte == 0xa5));
                assert!(memory[start + 9..start + 16].iter().all(|byte| *byte == 0));
            }
        }
    }

    #[test]
    fn naturally_packed_frame_preserves_all_samples() {
        let layout = FrameLayout::new(1279, 1706, JpuPixelFormat::Yuv420, JpuScale::Half)
            .expect("valid packed layout");
        let mut memory = std::vec![0xa5u8; layout.total_len];

        clear_frame_padding(&mut memory, &layout).expect("layout is valid");

        assert!(memory.iter().all(|byte| *byte == 0xa5));
    }
}
