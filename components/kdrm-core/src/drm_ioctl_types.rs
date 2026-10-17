// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_ioctl.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::size_of,
};

#[repr(C)]
pub struct DrmIoctlDesc {
    pub cmd: u32,
    pub flags: DrmIoctlFlags,
    pub func: *mut c_void,
    pub name: *mut c_char,
}

pub type DrmIoctlFlags = u32;
pub const DRM_AUTH: DrmIoctlFlags = 1;
pub const DRM_MASTER: DrmIoctlFlags = 2;
pub const DRM_ROOT_ONLY: DrmIoctlFlags = 4;
pub const DRM_RENDER_ALLOW: DrmIoctlFlags = 32;

unsafe impl Sync for DrmIoctlDesc {}
unsafe impl Send for DrmIoctlDesc {}

const _: () = assert!(size_of::<DrmIoctlDesc>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmIoctlDesc, cmd) == 0);
const _: () = assert!(core::mem::offset_of!(DrmIoctlDesc, flags) == 4);
const _: () = assert!(size_of::<DrmIoctlFlags>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmIoctlDesc, func) == 8);
const _: () = assert!(core::mem::offset_of!(DrmIoctlDesc, name) == 16);
