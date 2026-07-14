//! Shared ownership boundary for the single SG2002 JPU engine.

use ax_errno::AxError;
use ax_memory_addr::PhysAddr;
use ax_sync::Mutex;
use axfs_ng_vfs::VfsResult;
use dma_api::DmaError;
use sg200x_bsp::soc::TOP_BASE;
use sg200x_jpu::{
    FrameLayout, FrameLayoutError, JpuCreateError, JpuDecodeError, JpuDecoder, JpuMmio, JpuScale,
};
use starry_vm::vm_write_slice;

const JPU_REG_BASE: usize = 0x0b00_0000;
const VC_REG_BASE: usize = 0x0b03_0000;
const REG_MMIO_SIZE: usize = 0x1000;
const TOP_MMIO_SIZE: usize = 0x4000;

#[derive(Clone, Copy, Debug)]
pub(super) struct DecodedJpuFrame {
    pub layout: FrameLayout,
    pub dma_address: u64,
}

#[derive(Default)]
struct JpuState {
    decoder: Option<JpuDecoder>,
    vdec_owned: bool,
}

impl JpuState {
    fn decoder(&mut self) -> VfsResult<&mut JpuDecoder> {
        if self.decoder.is_none() {
            self.decoder = Some(create_decoder()?);
        }
        self.decoder.as_mut().ok_or(AxError::Io)
    }
}

/// Serializes the one SG2002 JPU between the legacy camera ioctl and VDEC.
pub(super) struct CviJpu {
    state: Mutex<JpuState>,
}

impl CviJpu {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(JpuState {
                decoder: None,
                vdec_owned: false,
            }),
        }
    }

    pub fn acquire_vdec(&self) -> VfsResult<()> {
        let mut state = self.state.lock();
        if state.vdec_owned {
            return Err(AxError::ResourceBusy);
        }
        state.decoder()?;
        state.vdec_owned = true;
        Ok(())
    }

    pub fn release_vdec(&self) {
        self.state.lock().vdec_owned = false;
    }

    pub fn decode_camera_to_user(&self, jpeg: &[u8], destination: *mut u8) -> VfsResult<usize> {
        let mut state = self.state.lock();
        if state.vdec_owned {
            return Err(AxError::ResourceBusy);
        }
        let result = state
            .decoder()?
            .decode(jpeg)
            .map_err(|error| map_decode_error(&error))?;
        vm_write_slice(destination, result.yuv_data)?;
        Ok(result.yuv_data.len())
    }

    pub fn decode_vdec(&self, jpeg: &[u8], scale: JpuScale) -> VfsResult<DecodedJpuFrame> {
        let mut state = self.state.lock();
        if !state.vdec_owned {
            return Err(AxError::InvalidInput);
        }
        let result = state
            .decoder()?
            .decode_scaled(jpeg, scale)
            .map_err(|error| map_decode_error(&error))?;
        Ok(DecodedJpuFrame {
            layout: result.layout,
            dma_address: u64::from(result.yuv_dma_addr),
        })
    }

    pub fn read_vdec_frame(
        &self,
        frame_len: usize,
        offset: usize,
        destination: &mut [u8],
    ) -> VfsResult<usize> {
        let state = self.state.lock();
        if !state.vdec_owned {
            return Err(AxError::InvalidInput);
        }
        let decoder = state.decoder.as_ref().ok_or(AxError::Io)?;
        decoder
            .copy_completed_frame(frame_len, offset, destination)
            .map_err(|error| map_decode_error(&error))
    }
}

fn map_mmio(physical: usize, size: usize) -> VfsResult<usize> {
    ax_mm::iomap(PhysAddr::from_usize(physical), size)
        .map(|address| address.as_usize())
        .map_err(|error| {
            warn!("cvi-jpu: failed to map MMIO at {physical:#x}+{size:#x}: {error:?}");
            AxError::Io
        })
}

fn create_decoder() -> VfsResult<JpuDecoder> {
    let mmio = JpuMmio::new(
        map_mmio(JPU_REG_BASE, REG_MMIO_SIZE)?,
        map_mmio(TOP_BASE, TOP_MMIO_SIZE)?,
        map_mmio(VC_REG_BASE, REG_MMIO_SIZE)?,
    );
    let dma = axklib::dma::device_with_mask(u32::MAX as u64);
    // SAFETY: the mappings above cover the documented JPU, TOP, and VC
    // register spans for the lifetime of this global service. `CviJpu` is the
    // sole accessor and serializes every decode through its mutex.
    unsafe { JpuDecoder::new(mmio, dma) }.map_err(map_create_error)
}

fn map_layout_error(_error: FrameLayoutError) -> AxError {
    AxError::OperationNotSupported
}

fn map_create_error(error: JpuCreateError) -> AxError {
    match error {
        JpuCreateError::AlreadyOwned => AxError::ResourceBusy,
        JpuCreateError::Initialization(message) => {
            warn!("cvi-jpu: initialization failed: {message}");
            AxError::Io
        }
    }
}

fn map_decode_error(error: &JpuDecodeError) -> AxError {
    warn!("cvi-jpu: decode failed: {error}");
    match error {
        JpuDecodeError::Layout(error) => map_layout_error(*error),
        JpuDecodeError::Dma(DmaError::NoMemory) => AxError::NoMemory,
        JpuDecodeError::Dma(_) => AxError::Io,
        JpuDecodeError::Timeout => AxError::TimedOut,
        JpuDecodeError::Poisoned
        | JpuDecodeError::EmptyStream
        | JpuDecodeError::InvalidJpeg(_)
        | JpuDecodeError::BufferInvariant(_)
        | JpuDecodeError::DmaAddress(_)
        | JpuDecodeError::HardwareSetup(_)
        | JpuDecodeError::DecodeFailed => AxError::Io,
    }
}
