// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_encoder.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::types::{ListHead, Mutex};

use crate::{
    drm_crtc_types::DrmCrtc, drm_device_types::DrmDevice, drm_mode_object_types::DrmModeObject,
    drm_modeset_helper_vtables_types::DrmEncoderHelperFuncs,
};

#[repr(C)]
pub struct DrmEncoderFuncs {
    pub reset: Option<unsafe extern "C" fn(*mut DrmEncoder)>,
    pub destroy: Option<unsafe extern "C" fn(*mut DrmEncoder)>,
    pub late_register: Option<unsafe extern "C" fn(*mut DrmEncoder) -> i32>,
    pub early_unregister: Option<unsafe extern "C" fn(*mut DrmEncoder)>,
    pub debugfs_init: Option<unsafe extern "C" fn(*mut DrmEncoder, *mut c_void)>,
}

#[repr(C, align(8))]
pub struct DrmEncoder {
    pub dev: *mut DrmDevice,
    pub head: ListHead,
    pub base: DrmModeObject,
    pub name: *mut c_char,
    pub encoder_type: i32,
    pub index: u32,
    pub possible_crtcs: u32,
    pub possible_clones: u32,
    pub crtc: *mut DrmCrtc,
    pub bridge_chain: ListHead,
    pub bridge_chain_mutex: Mutex,
    pub funcs: *mut DrmEncoderFuncs,
    pub helper_private: *mut DrmEncoderHelperFuncs,
    pub debugfs_entry: *mut c_void,
}

const _: () = assert!(size_of::<DrmEncoderFuncs>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmEncoderFuncs, reset) == 0);
const _: () = assert!(core::mem::offset_of!(DrmEncoderFuncs, destroy) == 8);
const _: () = assert!(core::mem::offset_of!(DrmEncoderFuncs, late_register) == 16);
const _: () = assert!(core::mem::offset_of!(DrmEncoderFuncs, early_unregister) == 24);
const _: () = assert!(core::mem::offset_of!(DrmEncoderFuncs, debugfs_init) == 32);
const _: () = assert!(size_of::<DrmEncoder>() == 152);
const _: () = assert!(align_of::<DrmEncoder>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, head) == 8);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, base) == 24);
const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, name) == 56);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, encoder_type) == 64);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, index) == 68);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, possible_crtcs) == 72);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, possible_clones) == 76);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, crtc) == 80);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, bridge_chain) == 88);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, bridge_chain_mutex) == 104);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, funcs) == 128);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, helper_private) == 136);
const _: () = assert!(core::mem::offset_of!(DrmEncoder, debugfs_entry) == 144);
