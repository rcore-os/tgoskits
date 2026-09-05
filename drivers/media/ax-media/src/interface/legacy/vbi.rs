//! Sliced VBI 能力结构。

use bitflags::bitflags;

/// Sliced VBI 能力。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SlicedVbiCap {
    pub service_set: VbiService,       // [out] 支持的服务集（位掩码）
    pub service_lines: [[u16; 24]; 2], // [out] 每场的服务行
    pub ty: u32,                       // [in] BufType 枚举
    pub reserved: [u32; 3],
}

// ── Sliced VBI 服务标志 — `V4L2_SLICED_*` ─────────────────────────

bitflags! {
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct VbiService: u16 {
        const TELETEXT_B  = 0x0001;
        const VPS         = 0x0400;
        const CAPTION_525 = 0x1000;
        const WSS_625     = 0x4000;
    }
}
