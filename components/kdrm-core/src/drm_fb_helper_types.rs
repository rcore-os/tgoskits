// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_fb_helper.h`

use core::mem::{align_of, size_of};

use drm_kapi::types::{Mutex, Spinlock, WorkStruct};

use crate::{
    drm_client_types::{DrmClientBuffer, DrmClientDev},
    drm_device_types::DrmDevice,
    drm_framebuffer_types::DrmFramebuffer,
    fb_types::{FbDeferredIo, FbInfo},
    uapi_drm_types::DrmClipRect,
};

#[repr(C)]
pub struct DrmFbHelperSurfaceSize {
    pub fb_width: u32,
    pub fb_height: u32,
    pub surface_width: u32,
    pub surface_height: u32,
    pub surface_bpp: u32,
    pub surface_depth: u32,
}

#[repr(C)]
pub struct DrmFbHelperFuncs {
    pub fb_dirty: Option<unsafe extern "C" fn(*mut DrmFbHelper, *mut DrmClipRect) -> i32>,
    pub fb_restore: Option<unsafe extern "C" fn(*mut DrmFbHelper)>,
    pub fb_set_suspend: Option<unsafe extern "C" fn(*mut DrmFbHelper, bool)>,
}

#[repr(C, align(8))]
pub struct DrmFbHelper {
    pub client: DrmClientDev,
    pub buffer: *mut DrmClientBuffer,
    pub fb: *mut DrmFramebuffer,
    pub dev: *mut DrmDevice,
    pub funcs: *mut DrmFbHelperFuncs,
    pub info: *mut FbInfo,
    pub pseudo_palette: [u32; 17],
    pub damage_clip: DrmClipRect,
    pub damage_lock: Spinlock,
    pub damage_work: WorkStruct,
    pub resume_work: WorkStruct,
    pub lock: Mutex,
    pub delayed_hotplug: bool,
    pub deferred_setup: bool,
    pub preferred_bpp: i32,
    pub fbdefio: FbDeferredIo,
}

const _: () = assert!(size_of::<DrmFbHelperSurfaceSize>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperSurfaceSize, fb_width) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperSurfaceSize, fb_height) == 4);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperSurfaceSize, surface_width) == 8);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperSurfaceSize, surface_height) == 12);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperSurfaceSize, surface_bpp) == 16);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperSurfaceSize, surface_depth) == 20);
const _: () = assert!(size_of::<DrmFbHelperFuncs>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperFuncs, fb_dirty) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperFuncs, fb_restore) == 8);
const _: () = assert!(core::mem::offset_of!(DrmFbHelperFuncs, fb_set_suspend) == 16);
const _: () = assert!(size_of::<DrmFbHelper>() == 336);
const _: () = assert!(align_of::<DrmFbHelper>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, client) == 0);
const _: () = assert!(size_of::<DrmClientDev>() == 88);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, buffer) == 88);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, fb) == 96);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, dev) == 104);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, funcs) == 112);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, info) == 120);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, pseudo_palette) == 128);
const _: () = assert!(size_of::<[u32; 17]>() == 68);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, damage_clip) == 196);
const _: () = assert!(size_of::<DrmClipRect>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, damage_lock) == 204);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, damage_work) == 208);
const _: () = assert!(size_of::<WorkStruct>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, resume_work) == 240);
const _: () = assert!(size_of::<WorkStruct>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, lock) == 272);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, delayed_hotplug) == 296);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, deferred_setup) == 297);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, preferred_bpp) == 300);
const _: () = assert!(core::mem::offset_of!(DrmFbHelper, fbdefio) == 304);
const _: () = assert!(size_of::<FbDeferredIo>() == 32);
