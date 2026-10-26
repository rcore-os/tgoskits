// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_auth.h`

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::{
    idr::Idr,
    types::{Kref, ListHead},
};

use crate::drm_device_types::DrmDevice;

#[repr(C, align(8))]
pub struct DrmMaster {
    pub refcount: Kref,
    pub dev: *mut DrmDevice,
    pub unique: *mut c_char,
    pub unique_len: i32,
    pub magic_map: Idr,
    pub driver_priv: *mut c_void,
    pub lessor: *mut DrmMaster,
    pub lessee_id: i32,
    pub lessee_list: ListHead,
    pub lessees: ListHead,
    pub leases: Idr,
    pub lessee_idr: Idr,
}

const _: () = assert!(size_of::<DrmMaster>() == 160);
const _: () = assert!(align_of::<DrmMaster>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmMaster, refcount) == 0);
const _: () = assert!(size_of::<Kref>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmMaster, dev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmMaster, unique) == 16);
const _: () = assert!(core::mem::offset_of!(DrmMaster, unique_len) == 24);
const _: () = assert!(core::mem::offset_of!(DrmMaster, magic_map) == 32);
const _: () = assert!(size_of::<Idr>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmMaster, driver_priv) == 56);
const _: () = assert!(core::mem::offset_of!(DrmMaster, lessor) == 64);
const _: () = assert!(core::mem::offset_of!(DrmMaster, lessee_id) == 72);
const _: () = assert!(core::mem::offset_of!(DrmMaster, lessee_list) == 80);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmMaster, lessees) == 96);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmMaster, leases) == 112);
const _: () = assert!(size_of::<Idr>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmMaster, lessee_idr) == 136);
const _: () = assert!(size_of::<Idr>() == 24);
