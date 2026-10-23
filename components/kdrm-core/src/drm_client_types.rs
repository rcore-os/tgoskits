// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_client.h`

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::types::{ListHead, Mutex};

use crate::{
    drm_crtc_types::DrmModeSet, drm_device_types::DrmDevice, drm_file_types::DrmFile,
    drm_framebuffer_types::DrmFramebuffer, drm_gem_types::DrmGemObject, iosys_map_types::IosysMap,
};

#[repr(C)]
pub struct DrmClientFuncs {
    pub owner: *mut c_void,
    pub free: Option<unsafe extern "C" fn(*mut DrmClientDev)>,
    pub unregister: Option<unsafe extern "C" fn(*mut DrmClientDev)>,
    pub restore: Option<unsafe extern "C" fn(*mut DrmClientDev, bool) -> i32>,
    pub hotplug: Option<unsafe extern "C" fn(*mut DrmClientDev) -> i32>,
    pub suspend: Option<unsafe extern "C" fn(*mut DrmClientDev) -> i32>,
    pub resume: Option<unsafe extern "C" fn(*mut DrmClientDev) -> i32>,
}

#[repr(C, align(8))]
pub struct DrmClientDev {
    pub dev: *mut DrmDevice,
    pub name: *mut c_char,
    pub list: ListHead,
    pub funcs: *mut DrmClientFuncs,
    pub file: *mut DrmFile,
    pub modeset_mutex: Mutex,
    pub modesets: *mut DrmModeSet,
    pub suspended: bool,
    pub hotplug_pending: bool,
    pub hotplug_failed: bool,
}

#[repr(C)]
pub struct DrmClientBuffer {
    pub client: *mut DrmClientDev,
    pub gem: *mut DrmGemObject,
    pub map: IosysMap,
    pub fb: *mut DrmFramebuffer,
}

const _: () = assert!(size_of::<DrmClientFuncs>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmClientFuncs, owner) == 0);
const _: () = assert!(core::mem::offset_of!(DrmClientFuncs, free) == 8);
const _: () = assert!(core::mem::offset_of!(DrmClientFuncs, unregister) == 16);
const _: () = assert!(core::mem::offset_of!(DrmClientFuncs, restore) == 24);
const _: () = assert!(core::mem::offset_of!(DrmClientFuncs, hotplug) == 32);
const _: () = assert!(core::mem::offset_of!(DrmClientFuncs, suspend) == 40);
const _: () = assert!(core::mem::offset_of!(DrmClientFuncs, resume) == 48);
const _: () = assert!(size_of::<DrmClientDev>() == 88);
const _: () = assert!(align_of::<DrmClientDev>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, name) == 8);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, list) == 16);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, funcs) == 32);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, file) == 40);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, modeset_mutex) == 48);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, modesets) == 72);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, suspended) == 80);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, hotplug_pending) == 81);
const _: () = assert!(core::mem::offset_of!(DrmClientDev, hotplug_failed) == 82);
const _: () = assert!(size_of::<DrmClientBuffer>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmClientBuffer, client) == 0);
const _: () = assert!(core::mem::offset_of!(DrmClientBuffer, gem) == 8);
const _: () = assert!(core::mem::offset_of!(DrmClientBuffer, map) == 16);
const _: () = assert!(size_of::<IosysMap>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmClientBuffer, fb) == 32);
