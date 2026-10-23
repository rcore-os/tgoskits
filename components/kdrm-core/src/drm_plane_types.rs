// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_plane.h`

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::types::{DmaFence, ListHead};

use crate::{
    drm_atomic_types::{DrmAtomicCommit, DrmCrtcCommit},
    drm_color_mgmt_types::{DrmColorEncoding, DrmColorRange},
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_framebuffer_types::DrmFramebuffer,
    drm_mode_object_types::{DrmModeObject, DrmObjectProperties},
    drm_modeset_helper_vtables_types::DrmPlaneHelperFuncs,
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    drm_print_types::DrmPrinter,
    drm_property_types::{DrmProperty, DrmPropertyBlob},
    drm_rect_types::DrmRect,
    kmsg_dump_types::KmsgDumper,
};

#[repr(C)]
pub struct DrmPlaneState {
    pub plane: *mut DrmPlane,
    pub crtc: *mut DrmCrtc,
    pub fb: *mut DrmFramebuffer,
    pub fence: *mut DmaFence,
    pub crtc_x: i32,
    pub crtc_y: i32,
    pub crtc_w: u32,
    pub crtc_h: u32,
    pub src_x: u32,
    pub src_y: u32,
    pub src_h: u32,
    pub src_w: u32,
    pub hotspot_x: i32,
    pub hotspot_y: i32,
    pub alpha: u16,
    pub pixel_blend_mode: u16,
    pub rotation: u32,
    pub zpos: u32,
    pub normalized_zpos: u32,
    pub color_encoding: DrmColorEncoding,
    pub color_range: DrmColorRange,
    pub fb_damage_clips: *mut DrmPropertyBlob,
    pub ignore_damage_clips: bool,
    pub src: DrmRect,
    pub dst: DrmRect,
    pub visible: bool,
    pub scaling_filter: DrmScalingFilter,
    pub color_pipeline: *mut c_void,
    pub commit: *mut DrmCrtcCommit,
    pub state: *mut DrmAtomicCommit,
    /// bit-fields color_mgmt_changed:1
    pub _bitfields_176: [u8; 1],
}

impl DrmPlaneState {
    #[inline]
    pub fn color_mgmt_changed(&self) -> bool {
        (self._bitfields_176[0] & 1) != 0
    }

    #[inline]
    pub fn set_color_mgmt_changed(&mut self, val: bool) {
        if val {
            self._bitfields_176[0] |= 1;
        } else {
            self._bitfields_176[0] &= !1;
        }
    }
}

/// Legacy plane-update callback; coordinates follow the DRM UAPI units.
pub type DrmPlaneUpdate = unsafe extern "C" fn(
    *mut DrmPlane,
    *mut DrmCrtc,
    *mut DrmFramebuffer,
    i32,
    i32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    *mut DrmModesetAcquireCtx,
) -> i32;

#[repr(C)]
pub struct DrmPlaneFuncs {
    pub update_plane: Option<DrmPlaneUpdate>,
    pub disable_plane:
        Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmModesetAcquireCtx) -> i32>,
    pub destroy: Option<unsafe extern "C" fn(*mut DrmPlane)>,
    pub reset: Option<unsafe extern "C" fn(*mut DrmPlane)>,
    pub set_property: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmProperty, u64) -> i32>,
    pub atomic_duplicate_state: Option<unsafe extern "C" fn(*mut DrmPlane) -> *mut DrmPlaneState>,
    pub atomic_destroy_state: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmPlaneState)>,
    pub atomic_set_property: Option<
        unsafe extern "C" fn(*mut DrmPlane, *mut DrmPlaneState, *mut DrmProperty, u64) -> i32,
    >,
    pub atomic_get_property: Option<
        unsafe extern "C" fn(*mut DrmPlane, *mut DrmPlaneState, *mut DrmProperty, *mut u64) -> i32,
    >,
    pub late_register: Option<unsafe extern "C" fn(*mut DrmPlane) -> i32>,
    pub early_unregister: Option<unsafe extern "C" fn(*mut DrmPlane)>,
    pub atomic_print_state: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut DrmPlaneState)>,
    pub format_mod_supported: Option<unsafe extern "C" fn(*mut DrmPlane, u32, u64) -> bool>,
    pub format_mod_supported_async: Option<unsafe extern "C" fn(*mut DrmPlane, u32, u64) -> bool>,
}

#[repr(C, align(8))]
pub struct DrmPlane {
    pub dev: *mut DrmDevice,
    pub head: ListHead,
    pub name: *mut c_char,
    pub mutex: DrmModesetLock,
    pub base: DrmModeObject,
    pub possible_crtcs: u32,
    pub format_types: *mut u32,
    pub format_count: u32,
    pub format_default: bool,
    pub modifiers: *mut u64,
    pub modifier_count: u32,
    pub crtc: *mut DrmCrtc,
    pub fb: *mut DrmFramebuffer,
    pub old_fb: *mut DrmFramebuffer,
    pub funcs: *mut DrmPlaneFuncs,
    pub properties: DrmObjectProperties,
    pub r#type: DrmPlaneType,
    pub index: u32,
    pub helper_private: *mut DrmPlaneHelperFuncs,
    pub state: *mut DrmPlaneState,
    pub alpha_property: *mut DrmProperty,
    pub zpos_property: *mut DrmProperty,
    pub rotation_property: *mut DrmProperty,
    pub blend_mode_property: *mut DrmProperty,
    pub color_encoding_property: *mut DrmProperty,
    pub color_range_property: *mut DrmProperty,
    pub color_pipeline_property: *mut DrmProperty,
    pub scaling_filter_property: *mut DrmProperty,
    pub hotspot_x_property: *mut DrmProperty,
    pub hotspot_y_property: *mut DrmProperty,
    pub kmsg_panic: KmsgDumper,
}

pub type DrmScalingFilter = u32;
pub const DRM_SCALING_FILTER_DEFAULT: DrmScalingFilter = 0;
pub const DRM_SCALING_FILTER_NEAREST_NEIGHBOR: DrmScalingFilter = 1;

pub type DrmPlaneType = u32;
pub const DRM_PLANE_TYPE_OVERLAY: DrmPlaneType = 0;
pub const DRM_PLANE_TYPE_PRIMARY: DrmPlaneType = 1;
pub const DRM_PLANE_TYPE_CURSOR: DrmPlaneType = 2;

const _: () = assert!(size_of::<DrmPlaneState>() == 184);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, plane) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, crtc) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, fb) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, fence) == 24);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, crtc_x) == 32);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, crtc_y) == 36);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, crtc_w) == 40);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, crtc_h) == 44);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, src_x) == 48);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, src_y) == 52);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, src_h) == 56);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, src_w) == 60);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, hotspot_x) == 64);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, hotspot_y) == 68);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, alpha) == 72);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, pixel_blend_mode) == 74);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, rotation) == 76);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, zpos) == 80);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, normalized_zpos) == 84);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, color_encoding) == 88);
const _: () = assert!(size_of::<DrmColorEncoding>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, color_range) == 92);
const _: () = assert!(size_of::<DrmColorRange>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, fb_damage_clips) == 96);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, ignore_damage_clips) == 104);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, src) == 108);
const _: () = assert!(size_of::<DrmRect>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, dst) == 124);
const _: () = assert!(size_of::<DrmRect>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, visible) == 140);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, scaling_filter) == 144);
const _: () = assert!(size_of::<DrmScalingFilter>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, color_pipeline) == 152);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, commit) == 160);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, state) == 168);
const _: () = assert!(core::mem::offset_of!(DrmPlaneState, _bitfields_176) == 176);
const _: () = assert!(size_of::<DrmPlaneFuncs>() == 112);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, update_plane) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, disable_plane) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, destroy) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, reset) == 24);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, set_property) == 32);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, atomic_duplicate_state) == 40);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, atomic_destroy_state) == 48);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, atomic_set_property) == 56);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, atomic_get_property) == 64);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, late_register) == 72);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, early_unregister) == 80);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, atomic_print_state) == 88);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, format_mod_supported) == 96);
const _: () = assert!(core::mem::offset_of!(DrmPlaneFuncs, format_mod_supported_async) == 104);
const _: () = assert!(size_of::<DrmPlane>() == 1352);
const _: () = assert!(align_of::<DrmPlane>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmPlane, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPlane, head) == 8);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPlane, name) == 24);
const _: () = assert!(core::mem::offset_of!(DrmPlane, mutex) == 32);
const _: () = assert!(size_of::<DrmModesetLock>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmPlane, base) == 80);
const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmPlane, possible_crtcs) == 112);
const _: () = assert!(core::mem::offset_of!(DrmPlane, format_types) == 120);
const _: () = assert!(core::mem::offset_of!(DrmPlane, format_count) == 128);
const _: () = assert!(core::mem::offset_of!(DrmPlane, format_default) == 132);
const _: () = assert!(core::mem::offset_of!(DrmPlane, modifiers) == 136);
const _: () = assert!(core::mem::offset_of!(DrmPlane, modifier_count) == 144);
const _: () = assert!(core::mem::offset_of!(DrmPlane, crtc) == 152);
const _: () = assert!(core::mem::offset_of!(DrmPlane, fb) == 160);
const _: () = assert!(core::mem::offset_of!(DrmPlane, old_fb) == 168);
const _: () = assert!(core::mem::offset_of!(DrmPlane, funcs) == 176);
const _: () = assert!(core::mem::offset_of!(DrmPlane, properties) == 184);
const _: () = assert!(size_of::<DrmObjectProperties>() == 1032);
const _: () = assert!(core::mem::offset_of!(DrmPlane, r#type) == 1216);
const _: () = assert!(size_of::<DrmPlaneType>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmPlane, index) == 1220);
const _: () = assert!(core::mem::offset_of!(DrmPlane, helper_private) == 1224);
const _: () = assert!(core::mem::offset_of!(DrmPlane, state) == 1232);
const _: () = assert!(core::mem::offset_of!(DrmPlane, alpha_property) == 1240);
const _: () = assert!(core::mem::offset_of!(DrmPlane, zpos_property) == 1248);
const _: () = assert!(core::mem::offset_of!(DrmPlane, rotation_property) == 1256);
const _: () = assert!(core::mem::offset_of!(DrmPlane, blend_mode_property) == 1264);
const _: () = assert!(core::mem::offset_of!(DrmPlane, color_encoding_property) == 1272);
const _: () = assert!(core::mem::offset_of!(DrmPlane, color_range_property) == 1280);
const _: () = assert!(core::mem::offset_of!(DrmPlane, color_pipeline_property) == 1288);
const _: () = assert!(core::mem::offset_of!(DrmPlane, scaling_filter_property) == 1296);
const _: () = assert!(core::mem::offset_of!(DrmPlane, hotspot_x_property) == 1304);
const _: () = assert!(core::mem::offset_of!(DrmPlane, hotspot_y_property) == 1312);
const _: () = assert!(core::mem::offset_of!(DrmPlane, kmsg_panic) == 1320);
const _: () = assert!(size_of::<KmsgDumper>() == 32);
