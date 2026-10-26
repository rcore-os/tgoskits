// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_drv.h`

use core::{
    ffi::{c_char, c_void},
    mem::size_of,
};

use drm_kapi::types::SgTable;

use crate::{
    drm_device_types::DrmDevice,
    drm_fb_helper_types::{DrmFbHelper, DrmFbHelperSurfaceSize},
    drm_file_types::{DrmFile, DrmMinor},
    drm_gem_types::DrmGemObject,
    drm_ioctl_types::DrmIoctlDesc,
    drm_print_types::DrmPrinter,
    uapi_drm_mode_types::DrmModeCreateDumb,
};

#[repr(C)]
pub struct DrmDriver {
    pub load: Option<unsafe extern "C" fn(*mut DrmDevice, u64) -> i32>,
    pub open: Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmFile) -> i32>,
    pub postclose: Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmFile)>,
    pub unload: Option<unsafe extern "C" fn(*mut DrmDevice)>,
    pub release: Option<unsafe extern "C" fn(*mut DrmDevice)>,
    pub master_set: Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmFile, bool)>,
    pub master_drop: Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmFile)>,
    pub debugfs_init: Option<unsafe extern "C" fn(*mut DrmMinor)>,
    pub gem_create_object: Option<unsafe extern "C" fn(*mut DrmDevice, usize) -> *mut DrmGemObject>,
    pub prime_handle_to_fd:
        Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmFile, u32, u32, *mut i32) -> i32>,
    pub prime_fd_to_handle:
        Option<unsafe extern "C" fn(*mut DrmDevice, *mut DrmFile, i32, *mut u32) -> i32>,
    pub gem_prime_import:
        Option<unsafe extern "C" fn(*mut DrmDevice, *mut c_void) -> *mut DrmGemObject>,
    pub gem_prime_import_sg_table: Option<
        unsafe extern "C" fn(*mut DrmDevice, *mut c_void, *mut SgTable) -> *mut DrmGemObject,
    >,
    pub dumb_create:
        Option<unsafe extern "C" fn(*mut DrmFile, *mut DrmDevice, *mut DrmModeCreateDumb) -> i32>,
    pub dumb_map_offset:
        Option<unsafe extern "C" fn(*mut DrmFile, *mut DrmDevice, u32, *mut u64) -> i32>,
    pub fbdev_probe:
        Option<unsafe extern "C" fn(*mut DrmFbHelper, *mut DrmFbHelperSurfaceSize) -> i32>,
    pub show_fdinfo: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut DrmFile)>,
    pub major: i32,
    pub minor: i32,
    pub patchlevel: i32,
    pub name: *mut c_char,
    pub desc: *mut c_char,
    pub driver_features: u32,
    pub ioctls: *mut DrmIoctlDesc,
    pub num_ioctls: i32,
    pub fops: *mut c_void,
}

const _: () = assert!(size_of::<DrmDriver>() == 200);
const _: () = assert!(core::mem::offset_of!(DrmDriver, load) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDriver, open) == 8);
const _: () = assert!(core::mem::offset_of!(DrmDriver, postclose) == 16);
const _: () = assert!(core::mem::offset_of!(DrmDriver, unload) == 24);
const _: () = assert!(core::mem::offset_of!(DrmDriver, release) == 32);
const _: () = assert!(core::mem::offset_of!(DrmDriver, master_set) == 40);
const _: () = assert!(core::mem::offset_of!(DrmDriver, master_drop) == 48);
const _: () = assert!(core::mem::offset_of!(DrmDriver, debugfs_init) == 56);
const _: () = assert!(core::mem::offset_of!(DrmDriver, gem_create_object) == 64);
const _: () = assert!(core::mem::offset_of!(DrmDriver, prime_handle_to_fd) == 72);
const _: () = assert!(core::mem::offset_of!(DrmDriver, prime_fd_to_handle) == 80);
const _: () = assert!(core::mem::offset_of!(DrmDriver, gem_prime_import) == 88);
const _: () = assert!(core::mem::offset_of!(DrmDriver, gem_prime_import_sg_table) == 96);
const _: () = assert!(core::mem::offset_of!(DrmDriver, dumb_create) == 104);
const _: () = assert!(core::mem::offset_of!(DrmDriver, dumb_map_offset) == 112);
const _: () = assert!(core::mem::offset_of!(DrmDriver, fbdev_probe) == 120);
const _: () = assert!(core::mem::offset_of!(DrmDriver, show_fdinfo) == 128);
const _: () = assert!(core::mem::offset_of!(DrmDriver, major) == 136);
const _: () = assert!(core::mem::offset_of!(DrmDriver, minor) == 140);
const _: () = assert!(core::mem::offset_of!(DrmDriver, patchlevel) == 144);
const _: () = assert!(core::mem::offset_of!(DrmDriver, name) == 152);
const _: () = assert!(core::mem::offset_of!(DrmDriver, desc) == 160);
const _: () = assert!(core::mem::offset_of!(DrmDriver, driver_features) == 168);
const _: () = assert!(core::mem::offset_of!(DrmDriver, ioctls) == 176);
const _: () = assert!(core::mem::offset_of!(DrmDriver, num_ioctls) == 184);
const _: () = assert!(core::mem::offset_of!(DrmDriver, fops) == 192);
