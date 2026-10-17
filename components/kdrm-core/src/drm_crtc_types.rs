// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_crtc.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::types::{ListHead, Spinlock};

use crate::{
    drm_atomic_types::{DrmAtomicCommit, DrmCrtcCommit},
    drm_connector_types::DrmConnector,
    drm_debugfs_crc_types::DrmCrtcCrc,
    drm_device_types::DrmDevice,
    drm_file_types::DrmFile,
    drm_framebuffer_types::DrmFramebuffer,
    drm_mode_object_types::{DrmModeObject, DrmObjectProperties},
    drm_modes_types::DrmDisplayMode,
    drm_modeset_helper_vtables_types::DrmCrtcHelperFuncs,
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    drm_plane_types::{DrmPlane, DrmScalingFilter},
    drm_print_types::DrmPrinter,
    drm_property_types::{DrmProperty, DrmPropertyBlob},
    drm_vblank_types::DrmPendingVblankEvent,
};

#[repr(C)]
pub struct DrmCrtcState {
    pub crtc: *mut DrmCrtc,
    pub enable: bool,
    pub active: bool,
    /// bit-fields planes_changed:1, mode_changed:1, active_changed:1, connectors_changed:1, zpos_changed:1, color_mgmt_changed:1
    pub _bitfields_10: [u8; 1],
    pub no_vblank: bool,
    pub plane_mask: u32,
    pub connector_mask: u32,
    pub encoder_mask: u32,
    pub adjusted_mode: DrmDisplayMode,
    pub mode: DrmDisplayMode,
    pub mode_blob: *mut DrmPropertyBlob,
    pub degamma_lut: *mut DrmPropertyBlob,
    pub ctm: *mut DrmPropertyBlob,
    pub gamma_lut: *mut DrmPropertyBlob,
    pub background_color: u64,
    pub target_vblank: u32,
    pub async_flip: bool,
    pub vrr_enabled: bool,
    pub self_refresh_active: bool,
    pub scaling_filter: DrmScalingFilter,
    pub sharpness_strength: u8,
    pub event: *mut DrmPendingVblankEvent,
    pub commit: *mut DrmCrtcCommit,
    pub state: *mut DrmAtomicCommit,
}

impl DrmCrtcState {
    #[inline]
    pub fn planes_changed(&self) -> bool {
        (self._bitfields_10[0] & 1) != 0
    }

    #[inline]
    pub fn set_planes_changed(&mut self, val: bool) {
        if val {
            self._bitfields_10[0] |= 1;
        } else {
            self._bitfields_10[0] &= !1;
        }
    }

    #[inline]
    pub fn mode_changed(&self) -> bool {
        (self._bitfields_10[0] & (1 << 1)) != 0
    }

    #[inline]
    pub fn set_mode_changed(&mut self, val: bool) {
        if val {
            self._bitfields_10[0] |= 1 << 1;
        } else {
            self._bitfields_10[0] &= !(1 << 1);
        }
    }

    #[inline]
    pub fn active_changed(&self) -> bool {
        (self._bitfields_10[0] & (1 << 2)) != 0
    }

    #[inline]
    pub fn set_active_changed(&mut self, val: bool) {
        if val {
            self._bitfields_10[0] |= 1 << 2;
        } else {
            self._bitfields_10[0] &= !(1 << 2);
        }
    }

    #[inline]
    pub fn connectors_changed(&self) -> bool {
        (self._bitfields_10[0] & (1 << 3)) != 0
    }

    #[inline]
    pub fn set_connectors_changed(&mut self, val: bool) {
        if val {
            self._bitfields_10[0] |= 1 << 3;
        } else {
            self._bitfields_10[0] &= !(1 << 3);
        }
    }

    #[inline]
    pub fn zpos_changed(&self) -> bool {
        (self._bitfields_10[0] & (1 << 4)) != 0
    }

    #[inline]
    pub fn set_zpos_changed(&mut self, val: bool) {
        if val {
            self._bitfields_10[0] |= 1 << 4;
        } else {
            self._bitfields_10[0] &= !(1 << 4);
        }
    }

    #[inline]
    pub fn color_mgmt_changed(&self) -> bool {
        (self._bitfields_10[0] & (1 << 5)) != 0
    }

    #[inline]
    pub fn set_color_mgmt_changed(&mut self, val: bool) {
        if val {
            self._bitfields_10[0] |= 1 << 5;
        } else {
            self._bitfields_10[0] &= !(1 << 5);
        }
    }
}

#[repr(C)]
pub struct DrmCrtcFuncs {
    pub reset: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub cursor_set: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmFile, u32, u32, u32) -> i32>,
    pub cursor_set2:
        Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmFile, u32, u32, u32, i32, i32) -> i32>,
    pub cursor_move: Option<unsafe extern "C" fn(*mut DrmCrtc, i32, i32) -> i32>,
    pub gamma_set: Option<
        unsafe extern "C" fn(
            *mut DrmCrtc,
            *mut u16,
            *mut u16,
            *mut u16,
            u32,
            *mut DrmModesetAcquireCtx,
        ) -> i32,
    >,
    pub destroy: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub set_config: Option<unsafe extern "C" fn(*mut DrmModeSet, *mut DrmModesetAcquireCtx) -> i32>,
    pub page_flip: Option<
        unsafe extern "C" fn(
            *mut DrmCrtc,
            *mut DrmFramebuffer,
            *mut DrmPendingVblankEvent,
            u32,
            *mut DrmModesetAcquireCtx,
        ) -> i32,
    >,
    pub page_flip_target: Option<
        unsafe extern "C" fn(
            *mut DrmCrtc,
            *mut DrmFramebuffer,
            *mut DrmPendingVblankEvent,
            u32,
            u32,
            *mut DrmModesetAcquireCtx,
        ) -> i32,
    >,
    pub set_property: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmProperty, u64) -> i32>,
    pub atomic_duplicate_state: Option<unsafe extern "C" fn(*mut DrmCrtc) -> *mut DrmCrtcState>,
    pub atomic_destroy_state: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmCrtcState)>,
    pub atomic_set_property:
        Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmCrtcState, *mut DrmProperty, u64) -> i32>,
    pub atomic_get_property: Option<
        unsafe extern "C" fn(*mut DrmCrtc, *mut DrmCrtcState, *mut DrmProperty, *mut u64) -> i32,
    >,
    pub late_register: Option<unsafe extern "C" fn(*mut DrmCrtc) -> i32>,
    pub early_unregister: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub set_crc_source: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut c_char) -> i32>,
    pub verify_crc_source:
        Option<unsafe extern "C" fn(*mut DrmCrtc, *mut c_char, *mut usize) -> i32>,
    pub get_crc_sources: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut usize) -> *mut c_char>,
    pub atomic_print_state: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut DrmCrtcState)>,
    pub get_vblank_counter: Option<unsafe extern "C" fn(*mut DrmCrtc) -> u32>,
    pub enable_vblank: Option<unsafe extern "C" fn(*mut DrmCrtc) -> i32>,
    pub disable_vblank: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub get_vblank_timestamp:
        Option<unsafe extern "C" fn(*mut DrmCrtc, *mut i32, *mut i64, bool) -> bool>,
}

#[repr(C, align(8))]
pub struct DrmCrtc {
    pub dev: *mut DrmDevice,
    pub port: *mut c_void,
    pub head: ListHead,
    pub name: *mut c_char,
    pub mutex: DrmModesetLock,
    pub base: DrmModeObject,
    pub primary: *mut DrmPlane,
    pub cursor: *mut DrmPlane,
    pub index: u32,
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub enabled: bool,
    pub mode: DrmDisplayMode,
    pub hwmode: DrmDisplayMode,
    pub x: i32,
    pub y: i32,
    pub funcs: *mut DrmCrtcFuncs,
    pub gamma_size: u32,
    pub gamma_store: *mut u16,
    pub helper_private: *mut DrmCrtcHelperFuncs,
    pub properties: DrmObjectProperties,
    pub scaling_filter_property: *mut DrmProperty,
    pub sharpness_strength_property: *mut DrmProperty,
    pub state: *mut DrmCrtcState,
    pub commit_list: ListHead,
    pub commit_lock: Spinlock,
    pub debugfs_entry: *mut c_void,
    pub crc: DrmCrtcCrc,
    pub fence_context: u32,
    pub fence_lock: Spinlock,
    pub fence_seqno: u64,
    pub timeline_name: [c_char; 32],
    pub self_refresh_data: *mut c_void,
}

#[repr(C)]
pub struct DrmModeSet {
    pub fb: *mut DrmFramebuffer,
    pub crtc: *mut DrmCrtc,
    pub mode: *mut DrmDisplayMode,
    pub x: u32,
    pub y: u32,
    pub connectors: *mut *mut DrmConnector,
    pub num_connectors: usize,
}

const _: () = assert!(size_of::<DrmCrtcState>() == 344);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, crtc) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, enable) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, active) == 9);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, _bitfields_10) == 10);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, no_vblank) == 11);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, plane_mask) == 12);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, connector_mask) == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, encoder_mask) == 20);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, adjusted_mode) == 24);
const _: () = assert!(size_of::<DrmDisplayMode>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, mode) == 144);
const _: () = assert!(size_of::<DrmDisplayMode>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, mode_blob) == 264);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, degamma_lut) == 272);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, ctm) == 280);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, gamma_lut) == 288);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, background_color) == 296);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, target_vblank) == 304);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, async_flip) == 308);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, vrr_enabled) == 309);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, self_refresh_active) == 310);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, scaling_filter) == 312);
const _: () = assert!(size_of::<DrmScalingFilter>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, sharpness_strength) == 316);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, event) == 320);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, commit) == 328);
const _: () = assert!(core::mem::offset_of!(DrmCrtcState, state) == 336);
const _: () = assert!(size_of::<DrmCrtcFuncs>() == 192);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, reset) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, cursor_set) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, cursor_set2) == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, cursor_move) == 24);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, gamma_set) == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, destroy) == 40);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, set_config) == 48);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, page_flip) == 56);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, page_flip_target) == 64);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, set_property) == 72);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, atomic_duplicate_state) == 80);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, atomic_destroy_state) == 88);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, atomic_set_property) == 96);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, atomic_get_property) == 104);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, late_register) == 112);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, early_unregister) == 120);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, set_crc_source) == 128);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, verify_crc_source) == 136);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, get_crc_sources) == 144);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, atomic_print_state) == 152);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, get_vblank_counter) == 160);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, enable_vblank) == 168);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, disable_vblank) == 176);
const _: () = assert!(core::mem::offset_of!(DrmCrtcFuncs, get_vblank_timestamp) == 184);
const _: () = assert!(size_of::<DrmCrtc>() == 1648);
const _: () = assert!(align_of::<DrmCrtc>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, port) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, head) == 16);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, name) == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, mutex) == 40);
const _: () = assert!(size_of::<DrmModesetLock>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, base) == 88);
const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, primary) == 120);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, cursor) == 128);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, index) == 136);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, cursor_x) == 140);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, cursor_y) == 144);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, enabled) == 148);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, mode) == 152);
const _: () = assert!(size_of::<DrmDisplayMode>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, hwmode) == 272);
const _: () = assert!(size_of::<DrmDisplayMode>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, x) == 392);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, y) == 396);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, funcs) == 400);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, gamma_size) == 408);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, gamma_store) == 416);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, helper_private) == 424);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, properties) == 432);
const _: () = assert!(size_of::<DrmObjectProperties>() == 1032);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, scaling_filter_property) == 1464);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, sharpness_strength_property) == 1472);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, state) == 1480);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, commit_list) == 1488);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, commit_lock) == 1504);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, debugfs_entry) == 1512);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, crc) == 1520);
const _: () = assert!(size_of::<DrmCrtcCrc>() == 72);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, fence_context) == 1592);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, fence_lock) == 1596);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, fence_seqno) == 1600);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, timeline_name) == 1608);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtc, self_refresh_data) == 1640);
const _: () = assert!(size_of::<DrmModeSet>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmModeSet, fb) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeSet, crtc) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeSet, mode) == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeSet, x) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModeSet, y) == 28);
const _: () = assert!(core::mem::offset_of!(DrmModeSet, connectors) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModeSet, num_connectors) == 40);
