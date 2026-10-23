// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_property.h`

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::types::ListHead;

use crate::{drm_device_types::DrmDevice, drm_mode_object_types::DrmModeObject};

#[repr(C)]
pub struct DrmPropertyEnum {
    pub value: u64,
    pub head: ListHead,
    pub name: [c_char; 32],
}

#[repr(C, align(8))]
pub struct DrmProperty {
    pub head: ListHead,
    pub base: DrmModeObject,
    pub flags: u32,
    pub name: [c_char; 32],
    pub num_values: u32,
    pub values: *mut u64,
    pub dev: *mut DrmDevice,
    pub enum_list: ListHead,
}

#[repr(C, align(8))]
pub struct DrmPropertyBlob {
    pub base: DrmModeObject,
    pub dev: *mut DrmDevice,
    pub head_global: ListHead,
    pub head_file: ListHead,
    pub length: usize,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct DrmPropEnumList {
    pub r#type: i32,
    pub name: *mut c_char,
}

const _: () = assert!(size_of::<DrmPropertyEnum>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmPropertyEnum, value) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPropertyEnum, head) == 8);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPropertyEnum, name) == 24);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(size_of::<DrmProperty>() == 120);
const _: () = assert!(align_of::<DrmProperty>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmProperty, head) == 0);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmProperty, base) == 16);
const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmProperty, flags) == 48);
const _: () = assert!(core::mem::offset_of!(DrmProperty, name) == 52);
const _: () = assert!(size_of::<[c_char; 32]>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmProperty, num_values) == 84);
const _: () = assert!(core::mem::offset_of!(DrmProperty, values) == 88);
const _: () = assert!(core::mem::offset_of!(DrmProperty, dev) == 96);
const _: () = assert!(core::mem::offset_of!(DrmProperty, enum_list) == 104);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(size_of::<DrmPropertyBlob>() == 88);
const _: () = assert!(align_of::<DrmPropertyBlob>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmPropertyBlob, base) == 0);
const _: () = assert!(size_of::<DrmModeObject>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmPropertyBlob, dev) == 32);
const _: () = assert!(core::mem::offset_of!(DrmPropertyBlob, head_global) == 40);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPropertyBlob, head_file) == 56);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPropertyBlob, length) == 72);
const _: () = assert!(core::mem::offset_of!(DrmPropertyBlob, data) == 80);
const _: () = assert!(size_of::<DrmPropEnumList>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPropEnumList, r#type) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPropEnumList, name) == 8);
