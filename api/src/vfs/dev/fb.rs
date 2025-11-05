use core::{any::Any, slice};

#[allow(unused_imports)]
use axdriver::prelude::DisplayDriverOps;
use axerrno::AxError;
use axfs_ng_vfs::{NodeFlags, VfsError, VfsResult};
use axhal::mem::virt_to_phys;
use memory_addr::{PhysAddrRange, VirtAddr};
use starry_core::vfs::{DeviceMmap, DeviceOps};
use starry_vm::VmMutPtr;

// Types from https://github.com/Tangzh33/asterinas

#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct FrameBufferBitfield {
    /// The beginning of bitfield.
    offset: u32,
    /// The length of bitfield.
    length: u32,
    /// Most significant bit is right(!= 0).
    msb_right: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
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
#[derive(Debug, Clone, Copy)]
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
    pub line_length: u32,   // Length of a line in bytes
    pub mmio_start: u64,    // Start of Memory Mapped I/O (physical address)
    pub mmio_len: u32,      // Length of Memory Mapped I/O
    pub accel: u32,         // Indicate to driver which specific chip/card we have
    pub capabilities: u16,  // See FB_CAP_*
    pub reserved: [u16; 2], // Reserved for future compatibility
}

async fn refresh_task() {
    let delay = core::time::Duration::from_secs_f32(1. / 60.);
    loop {
        if let Err(err) = axdisplay::main_display().flush() {
            warn!("Failed to refresh framebuffer: {err:?}");
        }
        axtask::future::sleep(delay).await;
    }
}

pub struct FrameBuffer {
    base: VirtAddr,
    size: usize,
}
impl FrameBuffer {
    pub fn new() -> Self {
        axtask::spawn(
            || axtask::future::block_on(refresh_task()),
            "fb-refresh".into(),
        );
        let info = axdisplay::main_display().info();
        Self {
            base: VirtAddr::from(info.fb_base_vaddr),
            size: info.fb_size,
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn as_mut_slice(&self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.base.as_mut_ptr(), self.size) }
    }
}
impl DeviceOps for FrameBuffer {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let slice = self.as_mut_slice();
        let len = buf
            .len()
            .min((slice.len() as u64).saturating_sub(offset) as usize);
        buf[..len].copy_from_slice(&slice[..len]);
        Ok(len)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> VfsResult<usize> {
        let slice = self.as_mut_slice();
        if offset >= slice.len() as u64 {
            return Err(VfsError::StorageFull);
        }
        let len = buf.len().min(slice.len() - offset as usize);
        slice[..len].copy_from_slice(&buf[..len]);
        Ok(len)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        match cmd {
            // FBIOGET_VSCREENINFO
            0x4600 => {
                let info = axdisplay::main_display().info();
                let line_length = (info.fb_size / info.height as usize) as u32;
                let bpp = line_length / info.width;
                (arg as *mut VarScreenInfo).vm_write(VarScreenInfo {
                    xres: info.width,
                    yres: info.height,
                    xres_virtual: info.width,
                    yres_virtual: info.height,
                    xoffset: 0,
                    yoffset: 0,
                    bits_per_pixel: bpp * 8,
                    grayscale: 0,
                    red: FrameBufferBitfield {
                        offset: 16,
                        length: 8,
                        msb_right: 0,
                    },
                    green: FrameBufferBitfield {
                        offset: 8,
                        length: 8,
                        msb_right: 0,
                    },
                    blue: FrameBufferBitfield {
                        offset: 0,
                        length: 8,
                        msb_right: 0,
                    },
                    transp: FrameBufferBitfield {
                        offset: 24,
                        length: 8,
                        msb_right: 0,
                    },
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
                })?;
                Ok(0)
            }
            // FBIOPUT_VSCREENINFO
            0x4601 => Ok(0),
            // FBIOGET_FSCREENINFO
            0x4602 => {
                let info = axdisplay::main_display().info();
                (arg as *mut FixScreenInfo).vm_write(FixScreenInfo {
                    id: *b"Virtio Framebuf\0",
                    smem_start: info.fb_base_vaddr as u64,
                    smem_len: info.fb_size as u32,
                    type_: 0,
                    type_aux: 0,
                    visual: 2, // FB_VISUAL_TRUECOLOR
                    xpanstep: 0,
                    ypanstep: 0,
                    ywrapstep: 0,
                    line_length: (info.fb_size / info.height as usize) as u32,
                    mmio_start: 0,
                    mmio_len: 0,
                    accel: 0,
                    capabilities: 0,
                    reserved: [0; 2],
                })?;
                Ok(0)
            }
            // FBIOGETCMAP
            0x4604 => Ok(0),
            // FBIOPUTCMAP
            0x4605 => Ok(0),
            // FBIOPAN_DISPLAY
            0x4606 => Err(AxError::InvalidInput),
            // FBIOBLANK
            0x4611 => Err(AxError::InvalidInput),
            _ => Err(AxError::NotATty),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mmap(&self) -> DeviceMmap {
        DeviceMmap::Physical(PhysAddrRange::from_start_size(
            virt_to_phys(self.base),
            self.size,
        ))
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE
    }
}
