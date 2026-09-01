//! JPEG 压缩控件（`V4L2_CTRL_CLASS_JPEG = 0x009d0000`）。

use super::CtrlClass;

/// `V4L2_CTRL_CLASS_JPEG` —— JPEG 压缩控件。
pub const CLASS_ID: u32 = CtrlClass::Jpeg as u32;

/// `V4L2_CID_JPEG_CLASS = (V4L2_CTRL_CLASS_JPEG | 1)`。
pub const CID_CLASS: u32 = CLASS_ID | 1;

/// `V4L2_CID_JPEG_CLASS_BASE = (V4L2_CTRL_CLASS_JPEG | 0x900) = 0x009d0900`。
pub const CID_BASE: u32 = CLASS_ID | 0x900;

/// `enum v4l2_jpeg_chroma_subsampling` —— `V4L2_CID_JPEG_CHROMA_SUBSAMPLING` 菜单项。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegChromaSubsampling {
    Sub444 = 0,
    Sub422 = 1,
    Sub420 = 2,
    Sub411 = 3,
    Sub410 = 4,
    Gray   = 5,
}

/// `V4L2_CID_JPEG_ACTIVE_MARKER` 的 `V4L2_JPEG_ACTIVE_MARKER_*` 位标志。
pub mod active_marker {
    pub const APP0: u32 = 1 << 0;
    pub const APP1: u32 = 1 << 1;
    pub const COM: u32 = 1 << 16;
    pub const DQT: u32 = 1 << 17;
    pub const DHT: u32 = 1 << 18;
}

/// V4L2 JPEG 类控制 ID（`V4L2_CID_JPEG_CLASS_BASE` + 偏移）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegClassCtrl {
    ChromaSubsampling  = CID_BASE + 1,
    RestartInterval    = CID_BASE + 2,
    CompressionQuality = CID_BASE + 3,
    ActiveMarker       = CID_BASE + 4,
}
