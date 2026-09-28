//! Overlay 帧缓冲结构。

use bitflags::bitflags;

/// 帧缓冲像素格式。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FbufFormat {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    pub colorspace: u32,
    pub priv_: u32,
}

/// 帧缓冲。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Framebuffer {
    pub capability: FbufCap, // [out] 帧缓冲能力（只读）
    pub flags: FbufFlags,    // [inout] 帧缓冲标志
    pub base: usize,         // [in] 帧缓冲物理地址
    pub fmt: FbufFormat,     // [inout] 帧缓冲像素格式
}

// ── 帧缓冲能力标志 ─────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FbufCap: u32 {
        const EXTERN_OVERLAY   = 0x0001;
        const CHROMAKEY        = 0x0002;
        const BITMAP_CLIPPING  = 0x0004;
        const LIST_CLIPPING    = 0x0008;
        const LOCAL_ALPHA      = 0x0010;
        const GLOBAL_ALPHA     = 0x0020;
        const LOCAL_INV_ALPHA  = 0x0040;
        const SRC_CHROMAKEY    = 0x0080;
    }
}

// ── 帧缓冲标志 ─────────────────────────────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FbufFlags: u32 {
        const PRIMARY         = 0x0001;
        const OVERLAY         = 0x0002;
        const CHROMAKEY       = 0x0004;
        const LOCAL_ALPHA     = 0x0008;
        const GLOBAL_ALPHA    = 0x0010;
        const LOCAL_INV_ALPHA = 0x0020;
        const SRC_CHROMAKEY   = 0x0040;
    }
}
