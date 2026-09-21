use core::ops::{Deref, DerefMut};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb565,
    Rgb888,
    Xrgb8888,
    Argb8888,
    Bgr888,
    Xbgr8888,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
    pub fb_size: usize,
}

pub struct FrameBuffer<'a> {
    raw: &'a mut [u8],
}

impl<'a> FrameBuffer<'a> {
    /// # Safety
    ///
    /// The caller must ensure that `ptr..ptr + len` is valid, uniquely
    /// borrowed for the lifetime `'a`, and points to framebuffer memory.
    pub unsafe fn from_raw_parts_mut(ptr: *mut u8, len: usize) -> Self {
        Self {
            raw: unsafe { core::slice::from_raw_parts_mut(ptr, len) },
        }
    }

    pub fn from_slice(slice: &'a mut [u8]) -> Self {
        Self { raw: slice }
    }

    pub fn as_slice(&self) -> &[u8] {
        self.raw
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.raw
    }
}

impl Deref for FrameBuffer<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.raw
    }
}

impl DerefMut for FrameBuffer<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.raw
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

/// Capset information returned by [`Interface::get_capset_info`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapsetInfo {
    pub capset_id: u32,
    pub max_version: u32,
    pub max_size: u32,
}

/// Guest-physical backing for a blob resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobMemory {
    pub paddr: u64,
    pub length: u32,
}

/// Parameters for creating a virtio-gpu 3D resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCreate3d {
    pub ctx_id: u32,
    pub resource_id: u32,
    pub target: u32,
    pub format: u32,
    pub bind: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
}

/// Parameters and initial command stream for creating a blob resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCreateBlob<'a> {
    pub ctx_id: u32,
    pub resource_id: u32,
    pub blob_mem: u32,
    pub blob_flags: u32,
    pub size: u64,
    pub blob_id: u64,
    pub backing: Option<BlobMemory>,
    pub cmd: &'a [u8],
}

/// Parameters for a virtio-gpu 3D transfer operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer3d {
    pub ctx_id: u32,
    pub resource_id: u32,
    pub box_: TransferBox,
    pub offset: u64,
    pub level: u32,
    pub stride: u32,
    pub layer_stride: u32,
}
