//! JPEG 压缩结构。

use bitflags::bitflags;

/// JPEG 压缩参数。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JpegCompression {
    pub quality: i32,              // [inout] 质量
    pub appn: i32,                 // [in] APP 段编号（0..15）
    pub app_len: i32,              // [in] APP 段数据长度
    pub app_data: [u8; 60],        // [in] APP 段数据
    pub com_len: i32,              // [in] COM 段数据长度
    pub com_data: [u8; 60],        // [in] COM 段数据
    pub jpeg_markers: JpegMarkers, // [in] 哪些标记写入 JPEG 输出
}

// ── JPEG 标记标志 — `V4L2_JPEG_MARKER_*` ─────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct JpegMarkers: u32 {
        const DHT  = 1 << 3;
        const DQT  = 1 << 4;
        const DRI  = 1 << 5;
        const COM  = 1 << 6;
        const APP  = 1 << 7;
    }
}
