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
