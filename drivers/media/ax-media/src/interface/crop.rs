//! 裁剪（cropping）、合成（composing）与 Selection 结构。
//!
//! Cropcap、Crop、Selection。

use bitflags::bitflags;

use crate::interface::{Fract, Rect};

/// 裁剪能力。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Cropcap {
    pub ty: u32,            // [in] BufType 枚举
    pub bounds: Rect,       // [out] 最大裁剪矩形
    pub defrect: Rect,      // [out] 默认裁剪矩形
    pub pixelaspect: Fract, // [out] 像素宽高比
}

/// 裁剪矩形。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Crop {
    pub ty: u32, // [in] BufType 枚举
    pub c: Rect, // [inout] 裁剪矩形（G_CROP：输出，S_CROP：输入）
}

/// Selection 目标 — `V4L2_SEL_TGT_*`（见 uapi/linux/v4l2-common.h）。
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionTarget {
    Crop           = 0x0000,
    CropDefault    = 0x0001,
    CropBounds     = 0x0002,
    NativeSize     = 0x0003,
    Compose        = 0x0100,
    ComposeDefault = 0x0101,
    ComposeBounds  = 0x0102,
    ComposePadded  = 0x0103,
}

impl SelectionTarget {
    /// 尝试将原始 `u32` 转换为 [`SelectionTarget`]。
    pub fn try_from_u32(v: u32) -> Option<Self> {
        Some(match v {
            0x0000 => Self::Crop,
            0x0001 => Self::CropDefault,
            0x0002 => Self::CropBounds,
            0x0003 => Self::NativeSize,
            0x0100 => Self::Compose,
            0x0101 => Self::ComposeDefault,
            0x0102 => Self::ComposeBounds,
            0x0103 => Self::ComposePadded,
            _ => return None,
        })
    }
}

bitflags! {
    /// Selection 约束标志 — `V4L2_SEL_FLAG_*`。
    #[repr(C)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SelectionFlags: u32 {
        const GE = 1 << 0;
        const LE = 1 << 1;
        const KEEP_CONFIG = 1 << 2;
    }
}

/// Selection 矩形（许多设备用它替代裁剪）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub ty: u32,                 // [in] BufType 枚举
    pub target: SelectionTarget, // [in] Selection 目标
    pub flags: SelectionFlags,   // [in] Selection 约束标志
    pub r: Rect,                 // [inout] Selection 矩形
    pub reserved: [u32; 9],
}
