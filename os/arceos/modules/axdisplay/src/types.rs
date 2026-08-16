/// The information of the graphics device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    /// The visible width.
    pub width: u32,
    /// The visible height.
    pub height: u32,
    /// The base virtual address of the framebuffer.
    pub fb_base_vaddr: usize,
    /// The size of the framebuffer in bytes.
    pub fb_size: usize,
    /// The number of framebuffer bytes per scanline.
    pub stride: usize,
    /// The framebuffer pixel layout.
    pub format: PixelFormat,
}

/// Pixel layouts used by framebuffer-backed display devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb565,
    Rgb888,
    Xrgb8888,
    Argb8888,
    Bgr888,
    Xbgr8888,
    Unknown,
}

impl DisplayInfo {
    /// Compatibility helper for callers that still infer pitch from size.
    pub fn line_length(&self) -> usize {
        if self.stride != 0 {
            self.stride
        } else if self.height == 0 {
            0
        } else {
            self.fb_size / self.height as usize
        }
    }
}

/// 3D box region for data transfer operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferBox {
    pub x: u32,
    pub y: u32,
    pub z: u32,
    pub w: u32,
    pub h: u32,
    pub d: u32,
}

/// Capset information returned by `gpu3d_capset_info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsetInfo {
    pub capset_id: u32,
    pub max_version: u32,
    pub max_size: u32,
}
