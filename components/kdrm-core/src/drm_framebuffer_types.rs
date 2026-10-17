// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_framebuffer.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::c_char,
    mem::{align_of, size_of},
};

use drm_kapi::types::ListHead;

use crate::{
    drm_device_types::DrmDevice, drm_file_types::DrmFile, drm_fourcc_types::DrmFormatInfo,
    drm_gem_types::DrmGemObject, drm_mode_object_types::DrmModeObject, uapi_drm_types::DrmClipRect,
};
pub const DRM_FORMAT_MAX_PLANES: usize = 4;

#[repr(C)]
pub struct DrmFramebufferFuncs {
    pub destroy: Option<unsafe extern "C" fn(*mut DrmFramebuffer)>,
    pub create_handle:
        Option<unsafe extern "C" fn(*mut DrmFramebuffer, *mut DrmFile, *mut u32) -> i32>,
    pub dirty: Option<
        unsafe extern "C" fn(
            *mut DrmFramebuffer,
            *mut DrmFile,
            u32,
            u32,
            *mut DrmClipRect,
            u32,
        ) -> i32,
    >,
}

#[repr(C, align(8))]
pub struct DrmFramebuffer {
    pub dev: *mut DrmDevice,
    pub head: ListHead,
    pub base: DrmModeObject,
    pub comm: [c_char; 16],
    pub format: *mut DrmFormatInfo,
    pub funcs: *mut DrmFramebufferFuncs,
    pub pitches: [u32; 4],
    pub offsets: [u32; 4],
    pub modifier: u64,
    pub width: u32,
    pub height: u32,
    pub flags: i32,
    pub internal_flags: u32,
    pub filp_head: ListHead,
    pub obj: [*mut DrmGemObject; 4],
}

#[repr(C, align(8))]
pub struct DrmAfbcFramebuffer {
    pub base: DrmFramebuffer,
    pub block_width: u32,
    pub block_height: u32,
    pub aligned_width: u32,
    pub aligned_height: u32,
    pub offset: u32,
    pub afbc_size: u32,
}

const _: () = assert!(size_of::<DrmFramebufferFuncs>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFramebufferFuncs, destroy) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFramebufferFuncs, create_handle) == 8);
const _: () = assert!(core::mem::offset_of!(DrmFramebufferFuncs, dirty) == 16);
const _: () = assert!(size_of::<DrmFramebuffer>() == 192);
const _: () = assert!(align_of::<DrmFramebuffer>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, head) == 8);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, base) == 24);
const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, comm) == 56);
const _: () = assert!(size_of::<[c_char; 16]>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, format) == 72);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, funcs) == 80);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, pitches) == 88);
const _: () = assert!(size_of::<[u32; 4]>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, offsets) == 104);
const _: () = assert!(size_of::<[u32; 4]>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, modifier) == 120);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, width) == 128);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, height) == 132);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, flags) == 136);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, internal_flags) == 140);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, filp_head) == 144);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFramebuffer, obj) == 160);
const _: () = assert!(size_of::<[*mut DrmGemObject; 4]>() == 32);
const _: () = assert!(size_of::<DrmAfbcFramebuffer>() == 216);
const _: () = assert!(align_of::<DrmAfbcFramebuffer>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmAfbcFramebuffer, base) == 0);
const _: () = assert!(size_of::<DrmFramebuffer>() == 192);
const _: () = assert!(core::mem::offset_of!(DrmAfbcFramebuffer, block_width) == 192);
const _: () = assert!(core::mem::offset_of!(DrmAfbcFramebuffer, block_height) == 196);
const _: () = assert!(core::mem::offset_of!(DrmAfbcFramebuffer, aligned_width) == 200);
const _: () = assert!(core::mem::offset_of!(DrmAfbcFramebuffer, aligned_height) == 204);
const _: () = assert!(core::mem::offset_of!(DrmAfbcFramebuffer, offset) == 208);
const _: () = assert!(core::mem::offset_of!(DrmAfbcFramebuffer, afbc_size) == 212);
