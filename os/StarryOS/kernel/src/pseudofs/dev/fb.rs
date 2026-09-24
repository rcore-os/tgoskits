use alloc::sync::Arc;
use core::any::Any;

use ax_gpu::MappableBacking;
use ax_display::PixelFormat;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};

use crate::{
    mm::VmMutPtr,
    pseudofs::{DeviceMmap, DeviceOps},
};

// Types from https://github.com/Tangzh33/asterinas

#[repr(C)]
#[derive(Default, Debug, Clone, Copy, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
pub struct FrameBufferBitfield {
    /// The beginning of bitfield.
    offset: u32,
    /// The length of bitfield.
    length: u32,
    /// Most significant bit is right(!= 0).
    msb_right: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
struct VarScreenInfo {
    pub xres: u32, // Visible resolution
    pub yres: u32,
    pub xres_virtual: u32, // Virtual resolution
    pub yres_virtual: u32,
    pub xoffset: u32, // Offset from virtual to visible
    pub yoffset: u32,
    pub bits_per_pixel: u32, // Guess what
    pub grayscale: u32,      // 0 = color, 1 = grayscale, >1 = FOURCC
    // Add other fields as needed
    pub red: FrameBufferBitfield, // Bitfield in framebuffer memory if true color
    pub green: FrameBufferBitfield, // Else only length is significant
    pub blue: FrameBufferBitfield,
    pub transp: FrameBufferBitfield, // Transparency
    pub nonstd: u32,                 // Non-standard pixel format
    pub activate: u32,               // See FB_ACTIVATE_*
    pub height: u32,                 // Height of picture in mm
    pub width: u32,                  // Width of picture in mm
    pub accel_flags: u32,            // (OBSOLETE) see fb_info.flags
    pub pixclock: u32,               // Pixel clock in ps (pico seconds)
    pub left_margin: u32,            // Time from sync to picture
    pub right_margin: u32,           // Time from picture to sync
    pub upper_margin: u32,           // Time from sync to picture
    pub lower_margin: u32,
    pub hsync_len: u32,     // Length of horizontal sync
    pub vsync_len: u32,     // Length of vertical sync
    pub sync: u32,          // See FB_SYNC_*
    pub vmode: u32,         // See FB_VMODE_*
    pub rotate: u32,        // Angle we rotate counter-clockwise
    pub colorspace: u32,    // Colorspace for FOURCC-based modes
    pub reserved: [u32; 4], // Reserved for future compatibility
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::AnyBitPattern, bytemuck::NoUninit)]
struct FixScreenInfo {
    pub id: [u8; 16],       // Identification string, e.g., "TT Builtin"
    pub smem_start: u64,    // Start of framebuffer memory (physical address)
    pub smem_len: u32,      // Length of framebuffer memory
    pub type_: u32,         // See FB_TYPE_*
    pub type_aux: u32,      // Interleave for interleaved planes
    pub visual: u32,        // See FB_VISUAL_*
    pub xpanstep: u16,      // Zero if no hardware panning
    pub ypanstep: u16,      // Zero if no hardware panning
    pub ywrapstep: u16,     // Zero if no hardware ywrap
    pub _padding0: u16,     // Explicit ABI alignment before line_length
    pub line_length: u32,   // Length of a line in bytes
    pub _padding1: u32,     // Explicit ABI alignment before mmio_start
    pub mmio_start: u64,    // Start of Memory Mapped I/O (physical address)
    pub mmio_len: u32,      // Length of Memory Mapped I/O
    pub accel: u32,         // Indicate to driver which specific chip/card we have
    pub capabilities: u16,  // See FB_CAP_*
    pub reserved: [u16; 2], // Reserved for future compatibility
    pub _padding2: u16,     // Explicit tail bytes in the 64-bit ABI
}

async fn refresh_task() {
    let delay = core::time::Duration::from_secs_f32(1. / 60.);
    loop {
        if ax_display::framebuffer_flush().is_err() {
            warn!("Failed to refresh framebuffer");
        }
        crate::task::future::sleep(delay).await;
    }
}

pub struct FrameBuffer {
    mapping: Arc<MappableBacking>,
}
impl FrameBuffer {
    pub fn new() -> Self {
        crate::task::kernel_thread_builder("fb-refresh".into())
            .spawn(|| crate::task::future::block_on(refresh_task()))
            .expect("failed to spawn kernel thread");
        let mapping = ax_display::framebuffer_mapping()
            .expect("display backing must exist when /dev/fb0 is registered");
        Self { mapping }
    }
}
impl DeviceOps for FrameBuffer {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let off = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        self.mapping.read_at(buf, off).map_err(|_| VfsError::Io)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let off = usize::try_from(offset).map_err(|_| VfsError::InvalidInput)?;
        self.mapping.write_at(buf, off).map_err(|error| match error {
            ax_gpu::rdif_gpu::GpuError::InvalidArgument => VfsError::StorageFull,
            _ => VfsError::Io,
        })
    }

    fn ioctl(&self, current: &crate::task::UserTaskRef, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            // FBIOGET_VSCREENINFO
            0x4600 => {
                let info = ax_display::framebuffer_info().map_err(|_| VfsError::Io)?;
                let bpp = info.format.bytes_per_pixel() as u32;
                let (red, green, blue, transp) = match info.format {
                    PixelFormat::Rgb565 => ((11, 5), (5, 6), (0, 5), (0, 0)),
                    PixelFormat::Rgb888 => ((16, 8), (8, 8), (0, 8), (0, 0)),
                    PixelFormat::Bgr888 => ((0, 8), (8, 8), (16, 8), (0, 0)),
                    PixelFormat::Xrgb8888 => ((16, 8), (8, 8), (0, 8), (0, 0)),
                    PixelFormat::Argb8888 => ((16, 8), (8, 8), (0, 8), (24, 8)),
                    PixelFormat::Xbgr8888 => ((0, 8), (8, 8), (16, 8), (0, 0)),
                };
                let bitfield = |(offset, length)| FrameBufferBitfield {
                    offset,
                    length,
                    msb_right: 0,
                };
                (arg as *mut VarScreenInfo)
                    .vm_write(
                        current,
                        VarScreenInfo {
                            xres: info.width,
                            yres: info.height,
                            xres_virtual: info.width,
                            yres_virtual: info.height,
                            xoffset: 0,
                            yoffset: 0,
                            bits_per_pixel: bpp * 8,
                            grayscale: 0,
                            red: bitfield(red),
                            green: bitfield(green),
                            blue: bitfield(blue),
                            transp: bitfield(transp),
                            nonstd: 0,
                            activate: 0,
                            height: 0,
                            width: 0,
                            accel_flags: 0,
                            pixclock: 10000000 / info.width * 1000 / info.height,
                            left_margin: (info.width / 8) & 0xf8,
                            right_margin: 32,
                            upper_margin: 16,
                            lower_margin: 4,
                            hsync_len: (info.width / 8) & 0xf8,
                            vsync_len: 4,
                            sync: 0,
                            vmode: 0,
                            rotate: 0,
                            colorspace: 0,
                            reserved: [0; 4],
                        },
                    )
                    .map_err(|error| VfsError::from(crate::StarryError::from(error)))?;
                Ok(0)
            }
            // FBIOPUT_VSCREENINFO
            0x4601 => Ok(0),
            // FBIOGET_FSCREENINFO
            0x4602 => {
                let info = ax_display::framebuffer_info().map_err(|_| VfsError::Io)?;
                let mut id = [0u8; 16];
                if let Some(identity) = ax_gpu::identity() {
                    let name = identity.device_name.as_bytes();
                    let len = name.len().min(id.len() - 1);
                    id[..len].copy_from_slice(&name[..len]);
                }
                let smem_start = self.mapping.physical().start.as_usize() as u64;
                (arg as *mut FixScreenInfo)
                    .vm_write(
                        current,
                        FixScreenInfo {
                            id,
                            smem_start,
                            smem_len: info.fb_size as u32,
                            type_: 0,
                            type_aux: 0,
                            visual: 2, // FB_VISUAL_TRUECOLOR
                            xpanstep: 0,
                            ypanstep: 0,
                            ywrapstep: 0,
                            _padding0: 0,
                            line_length: info.stride as u32,
                            _padding1: 0,
                            mmio_start: 0,
                            mmio_len: 0,
                            accel: 0,
                            capabilities: 0,
                            reserved: [0; 2],
                            _padding2: 0,
                        },
                    )
                    .map_err(|error| VfsError::from(crate::StarryError::from(error)))?;
                Ok(0)
            }
            // FBIOGETCMAP
            0x4604 => Ok(0),
            // FBIOPUTCMAP
            0x4605 => Ok(0),
            // FBIOPAN_DISPLAY
            0x4606 => Err(VfsError::InvalidInput),
            // FBIOBLANK
            0x4611 => Err(VfsError::InvalidInput),
            _ => Err(VfsError::NotATty),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mmap(&self, offset: u64, length: u64) -> DeviceMmap {
        let Some(end) = offset.checked_add(length) else {
            return DeviceMmap::None;
        };
        if end > self.mapping.physical().size() as u64 {
            return DeviceMmap::None;
        }
        let retainer: Arc<dyn Any + Send + Sync> = self.mapping.clone();
        DeviceMmap::Physical(
            self.mapping.physical(),
            Some(retainer),
        )
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}
