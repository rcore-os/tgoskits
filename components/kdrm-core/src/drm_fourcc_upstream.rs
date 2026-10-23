// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_fourcc.c`, including the
//! full `__drm_format_info()` table.
//!
//! Source-faithful transcription of Linux 7.2.3. The table below is the
//! little-endian (x86_64) form of the `formats[]` initializer; the
//! `#ifdef __BIG_ENDIAN` entries are excluded because the target is x86_64
//! only.

use drm_kapi::{
    assert::warn_on,
    print::{DrmLogLevel, drm_log_at},
};

use crate::{
    drm_device_types::DrmDevice,
    drm_fourcc_types::{DrmFormatInfo, DrmFormatInfoAnon6},
    uapi_drm_consts::*,
};

// Host-endian aliases (drm_fourcc.h). On little-endian hosts these are the
// plain formats.
pub const DRM_FORMAT_HOST_XRGB1555: u32 = DRM_FORMAT_XRGB1555;
pub const DRM_FORMAT_HOST_RGB565: u32 = DRM_FORMAT_RGB565;
pub const DRM_FORMAT_HOST_XRGB8888: u32 = DRM_FORMAT_XRGB8888;
pub const DRM_FORMAT_HOST_ARGB8888: u32 = DRM_FORMAT_ARGB8888;

// upstream: drm_fourcc.c formats[] (generated; little-endian, 134 entries)
/// Linux `__drm_format_info()` static table, generated from the
/// `formats[]` initializer in drm_fourcc.c (little-endian branch; the
/// `__BIG_ENDIAN` entries are not compiled on x86_64).
static FORMATS: [DrmFormatInfo; 134] = [
    DrmFormatInfo {
        format: DRM_FORMAT_C1,
        depth: 1,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [8, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: true,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_C2,
        depth: 2,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [4, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: true,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_C4,
        depth: 4,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [2, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: true,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_C8,
        depth: 8,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: true,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_D1,
        depth: 1,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [8, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_D2,
        depth: 2,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [4, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_D4,
        depth: 4,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [2, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_D8,
        depth: 8,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_R1,
        depth: 1,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [8, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_R2,
        depth: 2,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [4, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_R4,
        depth: 4,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [2, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_R8,
        depth: 8,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_R10,
        depth: 10,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_R12,
        depth: 12,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGB332,
        depth: 8,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGR233,
        depth: 8,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XRGB4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XBGR4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBX4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRX4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ARGB4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ABGR4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBA4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRA4444,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XRGB1555,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XBGR1555,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBX5551,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRX5551,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ARGB1555,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ABGR1555,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBA5551,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRA5551,
        depth: 15,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGB565,
        depth: 16,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGR565,
        depth: 16,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGB888,
        depth: 24,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [3, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGR888,
        depth: 24,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [3, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XRGB8888,
        depth: 24,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XBGR8888,
        depth: 24,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBX8888,
        depth: 24,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRX8888,
        depth: 24,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGB565_A8,
        depth: 24,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGR565_A8,
        depth: 24,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XRGB2101010,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XBGR2101010,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBX1010102,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRX1010102,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ARGB2101010,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ABGR2101010,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBA1010102,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRA1010102,
        depth: 30,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGB161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [6, 0, 0, 0],
        },
        block_w: [1, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGR161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [6, 0, 0, 0],
        },
        block_w: [1, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ARGB8888,
        depth: 32,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ABGR8888,
        depth: 32,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBA8888,
        depth: 32,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRA8888,
        depth: 32,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XRGB16161616F,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XBGR16161616F,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ARGB16161616F,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ABGR16161616F,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_AXBXGXRX106106106106,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XRGB16161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XBGR16161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ARGB16161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_ABGR16161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGB888_A8,
        depth: 32,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [3, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGR888_A8,
        depth: 32,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [3, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XRGB8888_A8,
        depth: 32,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XBGR8888_A8,
        depth: 32,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_RGBX8888_A8,
        depth: 32,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_BGRX8888_A8,
        depth: 32,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 1, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: false,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUV410,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 4,
        vsub: 4,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YVU410,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 4,
        vsub: 4,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUV411,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 4,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YVU411,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 4,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUV420,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YVU420,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUV422,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YVU422,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUV444,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YVU444,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 1, 1, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y8,
        depth: 8,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV12,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 2, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV21,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 2, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV16,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 2, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV61,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 2, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV24,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 2, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV42,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [1, 2, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUYV,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YVYU,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_UYVY,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_VYUY,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XYUV8888,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_VUY888,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [3, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XVUY2101010,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_AYUV,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y210,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y212,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y216,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y410,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y412,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y416,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: true,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XVYU2101010,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XVYU12_16161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XVYU16161616,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y0L0,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [2, 0, 0, 0],
        block_h: [2, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: true,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_X0L0,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [2, 0, 0, 0],
        block_h: [2, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Y0L2,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [2, 0, 0, 0],
        block_h: [2, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: true,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_X0L2,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [8, 0, 0, 0],
        },
        block_w: [2, 0, 0, 0],
        block_h: [2, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_P010,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 4, 0, 0],
        },
        block_w: [1, 1, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_P012,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 4, 0, 0],
        },
        block_w: [1, 1, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_P016,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 4, 0, 0],
        },
        block_w: [1, 1, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_P210,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 4, 0, 0],
        },
        block_w: [1, 1, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_VUY101010,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [0, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUV420_8BIT,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [0, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_YUV420_10BIT,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [0, 0, 0, 0],
        },
        block_w: [0, 0, 0, 0],
        block_h: [0, 0, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV15,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [5, 5, 0, 0],
        },
        block_w: [4, 2, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV20,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [5, 5, 0, 0],
        },
        block_w: [4, 2, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_NV30,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [5, 5, 0, 0],
        },
        block_w: [4, 2, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Q410,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_Q401,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_P030,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 8, 0, 0],
        },
        block_w: [3, 3, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_P230,
        depth: 0,
        num_planes: 2,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 8, 0, 0],
        },
        block_w: [3, 3, 0, 0],
        block_h: [1, 1, 0, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S010,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S210,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S410,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S012,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S212,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S412,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S016,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 2,
        vsub: 2,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S216,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 2,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_S416,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [2, 2, 2, 0],
        },
        block_w: [1, 1, 1, 0],
        block_h: [1, 1, 1, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_XYYY2101010,
        depth: 0,
        num_planes: 1,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 0, 0, 0],
        },
        block_w: [3, 0, 0, 0],
        block_h: [1, 0, 0, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
    DrmFormatInfo {
        format: DRM_FORMAT_T430,
        depth: 0,
        num_planes: 3,
        _anon_6: DrmFormatInfoAnon6 {
            char_per_block: [4, 4, 4, 0],
        },
        block_w: [3, 3, 3, 0],
        block_h: [1, 1, 1, 0],
        hsub: 1,
        vsub: 1,
        has_alpha: false,
        is_yuv: true,
        is_color_indexed: false,
    },
];

// upstream: drm_fourcc.c drm_mode_legacy_fb_format()
#[unsafe(no_mangle)]
pub extern "C" fn drm_mode_legacy_fb_format(bpp: u32, depth: u32) -> u32 {
    let mut fmt = DRM_FORMAT_INVALID;
    match bpp {
        1 => {
            if depth == 1 {
                fmt = DRM_FORMAT_C1;
            }
        }
        2 => {
            if depth == 2 {
                fmt = DRM_FORMAT_C2;
            }
        }
        4 => {
            if depth == 4 {
                fmt = DRM_FORMAT_C4;
            }
        }
        8 => {
            if depth == 8 {
                fmt = DRM_FORMAT_C8;
            }
        }
        16 => match depth {
            15 => fmt = DRM_FORMAT_XRGB1555,
            16 => fmt = DRM_FORMAT_RGB565,
            _ => {}
        },
        24 => {
            if depth == 24 {
                fmt = DRM_FORMAT_RGB888;
            }
        }
        32 => match depth {
            24 => fmt = DRM_FORMAT_XRGB8888,
            30 => fmt = DRM_FORMAT_XRGB2101010,
            32 => fmt = DRM_FORMAT_ARGB8888,
            _ => {}
        },
        _ => {}
    }
    fmt
}

// upstream: drm_fourcc.c drm_driver_legacy_fb_format()
#[unsafe(no_mangle)]
/// Returns the legacy framebuffer format for a given bpp and depth, taking driver quirks into account.
///
/// # Safety
/// - `dev` must be non-null, properly aligned, and point to an initialized `DrmDevice`.
/// - `(*dev).mode_config` must be initialized; its quirk flags (`quirk_addfb_prefer_host_byte_order`
///   and `quirk_addfb_prefer_xbgr_30bpp`) must be valid for concurrent immutable reads for the
///   duration of this call.
pub unsafe extern "C" fn drm_driver_legacy_fb_format(
    dev: *mut DrmDevice,
    bpp: u32,
    depth: u32,
) -> u32 {
    let mut fmt = drm_mode_legacy_fb_format(bpp, depth);
    // SAFETY: `dev` is guaranteed non-null, aligned, and live per caller preconditions; borrows
    // `(*dev).mode_config` immutably to read driver byte-order and format quirk flags.
    let mc = unsafe { &(*dev).mode_config };

    if mc.quirk_addfb_prefer_host_byte_order {
        if fmt == DRM_FORMAT_XRGB8888 {
            fmt = DRM_FORMAT_HOST_XRGB8888;
        }
        if fmt == DRM_FORMAT_ARGB8888 {
            fmt = DRM_FORMAT_HOST_ARGB8888;
        }
        if fmt == DRM_FORMAT_RGB565 {
            fmt = DRM_FORMAT_HOST_RGB565;
        }
        if fmt == DRM_FORMAT_XRGB1555 {
            fmt = DRM_FORMAT_HOST_XRGB1555;
        }
    }

    if mc.quirk_addfb_prefer_xbgr_30bpp && fmt == DRM_FORMAT_XRGB2101010 {
        fmt = DRM_FORMAT_XBGR2101010;
    }

    fmt
}

// upstream: drm_fourcc.c drm_driver_color_mode_format()
#[unsafe(no_mangle)]
/// Converts a legacy color depth mode into a standard DRM fourcc format.
///
/// # Safety
/// - `dev` must be non-null, properly aligned, and point to an initialized `DrmDevice` with
///   a valid `mode_config` structure, satisfying all invariants required by [`drm_driver_legacy_fb_format`].
pub unsafe extern "C" fn drm_driver_color_mode_format(dev: *mut DrmDevice, color_mode: u32) -> u32 {
    match color_mode {
        // SAFETY: forwards the valid `dev` pointer to `drm_driver_legacy_fb_format` with fixed 16 bpp and 15 depth for 15-bit RGB555 mode.
        15 => unsafe { drm_driver_legacy_fb_format(dev, 16, 15) },
        // SAFETY: forwards the valid `dev` pointer to `drm_driver_legacy_fb_format` with fixed 32 bpp and 24 depth for 32-bit XRGB8888 mode.
        32 => unsafe { drm_driver_legacy_fb_format(dev, 32, 24) },
        // SAFETY: forwards the valid `dev` pointer to `drm_driver_legacy_fb_format` using `color_mode` as both bpp and depth.
        _ => unsafe { drm_driver_legacy_fb_format(dev, color_mode, color_mode) },
    }
}

// upstream: drm_fourcc.c __drm_format_info()
#[unsafe(no_mangle)]
pub extern "C" fn __drm_format_info(format: u32) -> *const DrmFormatInfo {
    for info in FORMATS.iter() {
        if info.format == format {
            return info as *const DrmFormatInfo;
        }
    }
    core::ptr::null()
}

// upstream: drm_fourcc.c drm_format_info()
#[unsafe(no_mangle)]
pub extern "C" fn drm_format_info(format: u32) -> *const DrmFormatInfo {
    let info = __drm_format_info(format);
    warn_on(info.is_null());
    info
}

// upstream: drm_fourcc.c drm_get_format_info()
#[unsafe(no_mangle)]
/// Looks up format information, consulting driver-specific modifier hooks if present.
///
/// # Safety
/// - `dev` must not be null and must point to a live, properly aligned `DrmDevice`.
/// - `(*dev).mode_config.funcs` must be non-null and point to an initialized `DrmModeConfigFuncs`
///   vtable (as this function does not check for null before dereferencing `funcs`).
/// - If `(*funcs).get_format_info` is `Some(get)`, the callback must adhere to C ABI rules and be
///   safe to execute with `pixel_format` and `modifier`.
pub unsafe extern "C" fn drm_get_format_info(
    dev: *mut DrmDevice,
    pixel_format: u32,
    modifier: u64,
) -> *const DrmFormatInfo {
    let mut info: *const DrmFormatInfo = core::ptr::null();

    // SAFETY: `dev` is guaranteed non-null, properly aligned, and initialized by the caller contract,
    // permitting direct read access to its embedded `mode_config.funcs` pointer.
    let funcs = unsafe { (*dev).mode_config.funcs };
    // SAFETY: caller guarantees `funcs` is non-null and references an initialized `DrmModeConfigFuncs`
    // vtable, allowing the optional `get_format_info` function pointer to be loaded.
    if let Some(get) = unsafe { (*funcs).get_format_info } {
        // SAFETY: `get` is an active function pointer provided by the driver vtable; passing scalar
        // `pixel_format` and `modifier` fulfills its C ABI calling convention.
        info = unsafe { get(pixel_format, modifier) };
    }

    if info.is_null() {
        info = drm_format_info(pixel_format);
    }

    info
}

// upstream: drm_fourcc.c drm_format_info_block_width()
#[unsafe(no_mangle)]
/// Returns the block width in pixels for the specified plane of a format.
///
/// # Safety
/// If `info` is non-null, it must point to a live, properly aligned `DrmFormatInfo` structure
/// whose `block_w` array is readable for indices up to `num_planes` (at most 4).
/// Null `info` or invalid `plane` indices (`< 0` or `>= num_planes`) are safely detected and return 0.
pub unsafe extern "C" fn drm_format_info_block_width(
    info: *const DrmFormatInfo,
    plane: i32,
) -> u32 {
    // SAFETY: the short-circuiting `||` guarantees `info` is non-null before `(*info).num_planes` is dereferenced.
    if info.is_null() || plane < 0 || plane >= unsafe { (*info).num_planes } as i32 {
        return 0;
    }
    // SAFETY: `info` is non-null, aligned, and `plane` was bounds-checked to `[0, num_planes)`, which indexes into `block_w` (size 4).
    let bw = unsafe { (*info).block_w[plane as usize] };
    if bw == 0 {
        return 1;
    }
    bw as u32
}

// upstream: drm_fourcc.c drm_format_info_block_height()
#[unsafe(no_mangle)]
/// Returns the block height in pixels for the specified plane of a format.
///
/// # Safety
/// If `info` is non-null, it must point to a live, properly aligned `DrmFormatInfo` structure
/// whose `block_h` array is readable for indices up to `num_planes` (at most 4).
/// Null `info` or invalid `plane` indices (`< 0` or `>= num_planes`) are safely detected and return 0.
pub unsafe extern "C" fn drm_format_info_block_height(
    info: *const DrmFormatInfo,
    plane: i32,
) -> u32 {
    // SAFETY: `info` is verified non-null by the preceding checks in the logical OR expression before dereferencing `num_planes`.
    if info.is_null() || plane < 0 || plane >= unsafe { (*info).num_planes } as i32 {
        return 0;
    }
    // SAFETY: `info` is non-null, aligned, and `plane` was bounds-checked to `[0, num_planes)`, which indexes into `block_h` (size 4).
    let bh = unsafe { (*info).block_h[plane as usize] };
    if bh == 0 {
        return 1;
    }
    bh as u32
}

// upstream: drm_fourcc.c drm_format_info_bpp()
#[unsafe(no_mangle)]
/// Returns the bits per pixel for the specified plane of a format.
///
/// # Safety
/// If `info` is non-null, it must point to a live, properly aligned `DrmFormatInfo` structure
/// whose `block_w`, `block_h`, and `_anon_6.char_per_block` arrays are readable for the given `plane`.
/// Null `info`, invalid `plane` indices (`< 0` or `>= num_planes`), or formats without an integer
/// bits-per-pixel ratio safely return 0.
pub unsafe extern "C" fn drm_format_info_bpp(info: *const DrmFormatInfo, plane: i32) -> u32 {
    // SAFETY: the short-circuiting `||` confirms `info` is non-null before evaluating `(*info).num_planes`.
    if info.is_null() || plane < 0 || plane >= unsafe { (*info).num_planes } as i32 {
        return 0;
    }
    // SAFETY: `info` is non-null and `plane` is validated; delegates to `drm_format_info_block_width`.
    let block_size = unsafe { drm_format_info_block_width(info, plane) }
        // SAFETY: `info` is non-null and `plane` is validated; delegates to `drm_format_info_block_height`.
        * unsafe { drm_format_info_block_height(info, plane) };
    // SAFETY: `plane` is validated in `[0, num_planes)`, which safely indexes into `char_per_block` (length 4).
    let cpb = unsafe { (*info)._anon_6.char_per_block[plane as usize] } as u32;

    if !(cpb * 8).is_multiple_of(block_size) {
        drm_log_at(
            DrmLogLevel::Warn,
            "drm_fourcc",
            file!(),
            line!(),
            "unable to return an integer bpp",
        );
        return 0;
    }
    cpb * 8 / block_size
}

// upstream: drm_fourcc.c drm_format_info_min_pitch()
#[unsafe(no_mangle)]
/// Calculates the minimum line pitch in bytes for a given plane and buffer width.
///
/// # Safety
/// If `info` is non-null, it must point to a live, properly aligned `DrmFormatInfo` structure
/// whose `block_w`, `block_h`, and `_anon_6.char_per_block` arrays are readable for the given `plane`.
/// Null `info` or invalid `plane` indices (`< 0` or `>= num_planes`) are safely detected and return 0.
pub unsafe extern "C" fn drm_format_info_min_pitch(
    info: *const DrmFormatInfo,
    plane: i32,
    buffer_width: u32,
) -> u64 {
    // SAFETY: the short-circuiting condition prevents dereferencing `info` if it is null.
    if info.is_null() || plane < 0 || plane >= unsafe { (*info).num_planes } as i32 {
        return 0;
    }
    // SAFETY: `info` is non-null and `plane` is validated within `[0, num_planes)`, indexing `char_per_block` safely.
    let cpb = unsafe { (*info)._anon_6.char_per_block[plane as usize] } as u64;
    // SAFETY: passes validated non-null `info` and `plane` to helper functions; both return at least 1, avoiding division by zero.
    let div = unsafe {
        drm_format_info_block_width(info, plane) as u64
            * drm_format_info_block_height(info, plane) as u64
    };
    // DIV_ROUND_UP_ULL((u64)buffer_width * char_per_block, bw * bh)
    (buffer_width as u64 * cpb).div_ceil(div)
}
