// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_fourcc.h`

use core::mem::size_of;

#[repr(C)]
pub union DrmFormatInfoAnon6 {
    pub cpp: [u8; 4],
    pub char_per_block: [u8; 4],
}

#[repr(C)]
pub struct DrmFormatInfo {
    pub format: u32,
    pub depth: u8,
    pub num_planes: u8,
    pub _anon_6: DrmFormatInfoAnon6,
    pub block_w: [u8; 4],
    pub block_h: [u8; 4],
    pub hsub: u8,
    pub vsub: u8,
    pub has_alpha: bool,
    pub is_yuv: bool,
    pub is_color_indexed: bool,
}

const _: () = assert!(size_of::<DrmFormatInfoAnon6>() == 4);
const _: () = assert!(size_of::<DrmFormatInfo>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, format) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, depth) == 4);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, num_planes) == 5);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, _anon_6) == 6);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, block_w) == 10);
const _: () = assert!(size_of::<[u8; 4]>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, block_h) == 14);
const _: () = assert!(size_of::<[u8; 4]>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, hsub) == 18);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, vsub) == 19);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, has_alpha) == 20);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, is_yuv) == 21);
const _: () = assert!(core::mem::offset_of!(DrmFormatInfo, is_color_indexed) == 22);
