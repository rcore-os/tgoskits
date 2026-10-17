// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/display/drm_dp.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::mem::size_of;

#[repr(C)]
pub struct DpSdpHeader {
    pub HB0: u8,
    pub HB1: u8,
    pub HB2: u8,
    pub HB3: u8,
}

#[repr(C)]
pub struct DpSdp {
    pub sdp_header: DpSdpHeader,
    pub db: [u8; 32],
}

pub type DpPixelformat = u32;
pub const DP_PIXELFORMAT_RGB: DpPixelformat = 0;
pub const DP_PIXELFORMAT_YUV444: DpPixelformat = 1;
pub const DP_PIXELFORMAT_YUV422: DpPixelformat = 2;
pub const DP_PIXELFORMAT_YUV420: DpPixelformat = 3;
pub const DP_PIXELFORMAT_Y_ONLY: DpPixelformat = 4;
pub const DP_PIXELFORMAT_RAW: DpPixelformat = 5;
pub const DP_PIXELFORMAT_RESERVED: DpPixelformat = 6;

pub type DpColorimetry = u32;
pub const DP_COLORIMETRY_DEFAULT: DpColorimetry = 0;
pub const DP_COLORIMETRY_RGB_WIDE_FIXED: DpColorimetry = 1;
pub const DP_COLORIMETRY_BT709_YCC: DpColorimetry = 1;
pub const DP_COLORIMETRY_RGB_WIDE_FLOAT: DpColorimetry = 2;
pub const DP_COLORIMETRY_XVYCC_601: DpColorimetry = 2;
pub const DP_COLORIMETRY_OPRGB: DpColorimetry = 3;
pub const DP_COLORIMETRY_XVYCC_709: DpColorimetry = 3;
pub const DP_COLORIMETRY_DCI_P3_RGB: DpColorimetry = 4;
pub const DP_COLORIMETRY_SYCC_601: DpColorimetry = 4;
pub const DP_COLORIMETRY_RGB_CUSTOM: DpColorimetry = 5;
pub const DP_COLORIMETRY_OPYCC_601: DpColorimetry = 5;
pub const DP_COLORIMETRY_BT2020_RGB: DpColorimetry = 6;
pub const DP_COLORIMETRY_BT2020_CYCC: DpColorimetry = 6;
pub const DP_COLORIMETRY_BT2020_YCC: DpColorimetry = 7;

pub type DpDynamicRange = u32;
pub const DP_DYNAMIC_RANGE_VESA: DpDynamicRange = 0;
pub const DP_DYNAMIC_RANGE_CTA: DpDynamicRange = 1;

pub type DpContentType = u32;
pub const DP_CONTENT_TYPE_NOT_DEFINED: DpContentType = 0;
pub const DP_CONTENT_TYPE_GRAPHICS: DpContentType = 1;
pub const DP_CONTENT_TYPE_PHOTO: DpContentType = 2;
pub const DP_CONTENT_TYPE_VIDEO: DpContentType = 3;
pub const DP_CONTENT_TYPE_GAME: DpContentType = 4;

pub type OperationMode = u32;
pub const DP_AS_SDP_AVT_DYNAMIC_VTOTAL: OperationMode = 0;
pub const DP_AS_SDP_AVT_FIXED_VTOTAL: OperationMode = 1;
pub const DP_AS_SDP_FAVT_TRR_NOT_REACHED: OperationMode = 2;
pub const DP_AS_SDP_FAVT_TRR_REACHED: OperationMode = 3;

const _: () = assert!(size_of::<DpSdpHeader>() == 4);
const _: () = assert!(core::mem::offset_of!(DpSdpHeader, HB0) == 0);
const _: () = assert!(core::mem::offset_of!(DpSdpHeader, HB1) == 1);
const _: () = assert!(core::mem::offset_of!(DpSdpHeader, HB2) == 2);
const _: () = assert!(core::mem::offset_of!(DpSdpHeader, HB3) == 3);
const _: () = assert!(size_of::<DpSdp>() == 36);
const _: () = assert!(core::mem::offset_of!(DpSdp, sdp_header) == 0);
const _: () = assert!(size_of::<DpSdpHeader>() == 4);
const _: () = assert!(core::mem::offset_of!(DpSdp, db) == 4);
const _: () = assert!(size_of::<[u8; 32]>() == 32);
