// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_mode_object.h`

use core::mem::{align_of, size_of};

use drm_kapi::types::Kref;

use crate::drm_property_types::DrmProperty;

#[repr(C, align(8))]
pub struct DrmModeObject {
    pub id: u32,
    pub r#type: u32,
    pub properties: *mut DrmObjectProperties,
    pub refcount: Kref,
    pub free_cb: Option<unsafe extern "C" fn(*mut Kref)>,
}

#[repr(C)]
pub struct DrmObjectProperties {
    pub count: i32,
    pub properties: [*mut DrmProperty; 64],
    pub values: [u64; 64],
}

const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(align_of::<DrmModeObject>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeObject, id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeObject, r#type) == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeObject, properties) == 8);
const _: () = assert!(core::mem::offset_of!(DrmModeObject, refcount) == 16);
const _: () = assert!(size_of::<Kref>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmModeObject, free_cb) == 24);
const _: () = assert!(size_of::<DrmObjectProperties>() == 1032);
const _: () = assert!(core::mem::offset_of!(DrmObjectProperties, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmObjectProperties, properties) == 8);
const _: () = assert!(size_of::<[*mut DrmProperty; 64]>() == 512);
const _: () = assert!(core::mem::offset_of!(DrmObjectProperties, values) == 520);
const _: () = assert!(size_of::<[u64; 64]>() == 512);
