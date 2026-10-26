// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Linux v7.2.3 include/drm/drm_gem_shmem_helper.h, defined from its source
// field order and x86_64 ABI. The three Linux bool bitfields share one byte.
//! Linux DRM shmem-helper object definition.

use core::{
    ffi::c_void,
    mem::{align_of, offset_of, size_of},
};

use drm_kapi::types::{ListHead, Page, RefcountT, SgTable};

use crate::drm_gem_types::DrmGemObject;

#[repr(C, align(8))]
pub struct DrmGemShmemObject {
    pub base: DrmGemObject,
    pub pages: *mut *mut Page,
    pub pages_use_count: RefcountT,
    pub pages_pin_count: RefcountT,
    pub madv: i32,
    pub madv_list: ListHead,
    pub sgt: *mut SgTable,
    pub vaddr: *mut c_void,
    pub vmap_use_count: RefcountT,
    /// Linux `pages_mark_dirty_on_put`, `pages_mark_accessed_on_put`, and
    /// `map_wc` one-bit fields, in declaration order.
    pub flags: u8,
}

pub const DRM_GEM_SHMEM_PAGES_MARK_DIRTY_ON_PUT: u8 = 1 << 0;
pub const DRM_GEM_SHMEM_PAGES_MARK_ACCESSED_ON_PUT: u8 = 1 << 1;
pub const DRM_GEM_SHMEM_MAP_WC: u8 = 1 << 2;
pub const DRM_GEM_SHMEM_PURGED: i32 = -1;

const _: () = assert!(align_of::<DrmGemShmemObject>() == 8);
const _: () = assert!(offset_of!(DrmGemShmemObject, base) == 0);
const _: () = assert!(offset_of!(DrmGemShmemObject, pages) == size_of::<DrmGemObject>());
const _: () =
    assert!(offset_of!(DrmGemShmemObject, pages_use_count) == size_of::<DrmGemObject>() + 8);
const _: () =
    assert!(offset_of!(DrmGemShmemObject, pages_pin_count) == size_of::<DrmGemObject>() + 12);
const _: () = assert!(offset_of!(DrmGemShmemObject, madv) == size_of::<DrmGemObject>() + 16);
const _: () = assert!(offset_of!(DrmGemShmemObject, madv_list) == size_of::<DrmGemObject>() + 24);
const _: () = assert!(offset_of!(DrmGemShmemObject, sgt) == size_of::<DrmGemObject>() + 40);
const _: () = assert!(offset_of!(DrmGemShmemObject, vaddr) == size_of::<DrmGemObject>() + 48);
const _: () =
    assert!(offset_of!(DrmGemShmemObject, vmap_use_count) == size_of::<DrmGemObject>() + 56);
const _: () = assert!(offset_of!(DrmGemShmemObject, flags) == size_of::<DrmGemObject>() + 60);
const _: () = assert!(size_of::<DrmGemShmemObject>() == size_of::<DrmGemObject>() + 64);
