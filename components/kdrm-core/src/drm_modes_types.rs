// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_modes.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{ffi::c_char, mem::size_of};

use drm_kapi::types::ListHead;

use crate::hdmi_types::HdmiPictureAspect;

#[repr(C)]
pub struct DrmDisplayMode {
    pub clock: i32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub flags: u32,
    pub crtc_clock: i32,
    pub crtc_hdisplay: u16,
    pub crtc_hblank_start: u16,
    pub crtc_hblank_end: u16,
    pub crtc_hsync_start: u16,
    pub crtc_hsync_end: u16,
    pub crtc_htotal: u16,
    pub crtc_hskew: u16,
    pub crtc_vdisplay: u16,
    pub crtc_vblank_start: u16,
    pub crtc_vblank_end: u16,
    pub crtc_vsync_start: u16,
    pub crtc_vsync_end: u16,
    pub crtc_vtotal: u16,
    pub width_mm: u16,
    pub height_mm: u16,
    pub r#type: u8,
    pub expose_to_userspace: bool,
    pub head: ListHead,
    pub name: [c_char; 32],
    pub status: DrmModeStatus,
    pub picture_aspect_ratio: HdmiPictureAspect,
}

pub type DrmModeStatus = i32;
pub const MODE_OK: DrmModeStatus = 0;
pub const MODE_HSYNC: DrmModeStatus = 1;
pub const MODE_VSYNC: DrmModeStatus = 2;
pub const MODE_H_ILLEGAL: DrmModeStatus = 3;
pub const MODE_V_ILLEGAL: DrmModeStatus = 4;
pub const MODE_BAD_WIDTH: DrmModeStatus = 5;
pub const MODE_NOMODE: DrmModeStatus = 6;
pub const MODE_NO_INTERLACE: DrmModeStatus = 7;
pub const MODE_NO_DBLESCAN: DrmModeStatus = 8;
pub const MODE_NO_VSCAN: DrmModeStatus = 9;
pub const MODE_MEM: DrmModeStatus = 10;
pub const MODE_VIRTUAL_X: DrmModeStatus = 11;
pub const MODE_VIRTUAL_Y: DrmModeStatus = 12;
pub const MODE_MEM_VIRT: DrmModeStatus = 13;
pub const MODE_NOCLOCK: DrmModeStatus = 14;
pub const MODE_CLOCK_HIGH: DrmModeStatus = 15;
pub const MODE_CLOCK_LOW: DrmModeStatus = 16;
pub const MODE_CLOCK_RANGE: DrmModeStatus = 17;
pub const MODE_BAD_HVALUE: DrmModeStatus = 18;
pub const MODE_BAD_VVALUE: DrmModeStatus = 19;
pub const MODE_BAD_VSCAN: DrmModeStatus = 20;
pub const MODE_HSYNC_NARROW: DrmModeStatus = 21;
pub const MODE_HSYNC_WIDE: DrmModeStatus = 22;
pub const MODE_HBLANK_NARROW: DrmModeStatus = 23;
pub const MODE_HBLANK_WIDE: DrmModeStatus = 24;
pub const MODE_VSYNC_NARROW: DrmModeStatus = 25;
pub const MODE_VSYNC_WIDE: DrmModeStatus = 26;
pub const MODE_VBLANK_NARROW: DrmModeStatus = 27;
pub const MODE_VBLANK_WIDE: DrmModeStatus = 28;
pub const MODE_PANEL: DrmModeStatus = 29;
pub const MODE_INTERLACE_WIDTH: DrmModeStatus = 30;
pub const MODE_ONE_WIDTH: DrmModeStatus = 31;
pub const MODE_ONE_HEIGHT: DrmModeStatus = 32;
pub const MODE_ONE_SIZE: DrmModeStatus = 33;
pub const MODE_NO_REDUCED: DrmModeStatus = 34;
pub const MODE_NO_STEREO: DrmModeStatus = 35;
pub const MODE_NO_420: DrmModeStatus = 36;
pub const MODE_STALE: DrmModeStatus = -3;
pub const MODE_BAD: DrmModeStatus = -2;
pub const MODE_ERROR: DrmModeStatus = -1;

const _: () = assert!(size_of::<DrmDisplayMode>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, clock) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, hdisplay) == 4);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, hsync_start) == 6);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, hsync_end) == 8);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, htotal) == 10);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, hskew) == 12);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, vdisplay) == 14);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, vsync_start) == 16);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, vsync_end) == 18);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, vtotal) == 20);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, vscan) == 22);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, flags) == 24);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_clock) == 28);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_hdisplay) == 32);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_hblank_start) == 34);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_hblank_end) == 36);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_hsync_start) == 38);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_hsync_end) == 40);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_htotal) == 42);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_hskew) == 44);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_vdisplay) == 46);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_vblank_start) == 48);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_vblank_end) == 50);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_vsync_start) == 52);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_vsync_end) == 54);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, crtc_vtotal) == 56);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, width_mm) == 58);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, height_mm) == 60);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, r#type) == 62);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, expose_to_userspace) == 63);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, head) == 64);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, name) == 80);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, status) == 112);
const _: () = assert!(size_of::<DrmModeStatus>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDisplayMode, picture_aspect_ratio) == 116);
const _: () = assert!(size_of::<HdmiPictureAspect>() == 4);
