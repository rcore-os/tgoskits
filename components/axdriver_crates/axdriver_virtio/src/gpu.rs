use ax_driver_base::{BaseDriverOps, DevResult, DeviceType};
use ax_driver_display::{DisplayDriverOps, DisplayInfo, FrameBuffer};
use virtio_drivers::{Hal, device::gpu::VirtIOGpu as InnerDev, transport::Transport};

use crate::as_dev_err;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameBufferState {
    base_vaddr: usize,
    size: usize,
}

impl FrameBufferState {
    const fn new(base_vaddr: usize, size: usize) -> Self {
        Self { base_vaddr, size }
    }

    const fn base_vaddr(self) -> usize {
        self.base_vaddr
    }

    const fn size(self) -> usize {
        self.size
    }
}

/// The VirtIO GPU device driver.
pub struct VirtIoGpuDev<H: Hal, T: Transport> {
    inner: InnerDev<H, T>,
    info: DisplayInfo,
}

unsafe impl<H: Hal, T: Transport> Send for VirtIoGpuDev<H, T> {}
unsafe impl<H: Hal, T: Transport> Sync for VirtIoGpuDev<H, T> {}

impl<H: Hal, T: Transport> VirtIoGpuDev<H, T> {
    fn setup_framebuffer_state(virtio: &mut InnerDev<H, T>) -> DevResult<FrameBufferState> {
        let framebuffer = virtio.setup_framebuffer().map_err(as_dev_err)?;
        Ok(FrameBufferState::new(
            framebuffer.as_mut_ptr() as usize,
            framebuffer.len(),
        ))
    }

    fn read_resolution(virtio: &mut InnerDev<H, T>) -> DevResult<(u32, u32)> {
        virtio.resolution().map_err(as_dev_err)
    }

    fn build_display_info(framebuffer: FrameBufferState, width: u32, height: u32) -> DisplayInfo {
        DisplayInfo {
            width,
            height,
            fb_base_vaddr: framebuffer.base_vaddr(),
            fb_size: framebuffer.size(),
        }
    }

    /// Creates a new driver instance and initializes the device, or returns
    /// an error if any step fails.
    pub fn try_new(transport: T) -> DevResult<Self> {
        let mut virtio = InnerDev::new(transport).map_err(as_dev_err)?;
        let framebuffer = Self::setup_framebuffer_state(&mut virtio)?;
        let (width, height) = Self::read_resolution(&mut virtio)?;
        let info = Self::build_display_info(framebuffer, width, height);

        Ok(Self {
            inner: virtio,
            info,
        })
    }
}

impl<H: Hal, T: Transport> BaseDriverOps for VirtIoGpuDev<H, T> {
    fn device_name(&self) -> &str {
        "virtio-gpu"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Display
    }
}

impl<H: Hal, T: Transport> DisplayDriverOps for VirtIoGpuDev<H, T> {
    fn info(&self) -> DisplayInfo {
        self.info
    }

    fn fb(&self) -> FrameBuffer<'_> {
        unsafe {
            FrameBuffer::from_raw_parts_mut(self.info.fb_base_vaddr as *mut u8, self.info.fb_size)
        }
    }

    fn need_flush(&self) -> bool {
        true
    }

    fn flush(&mut self) -> DevResult {
        self.inner.flush().map_err(as_dev_err)
    }
}
