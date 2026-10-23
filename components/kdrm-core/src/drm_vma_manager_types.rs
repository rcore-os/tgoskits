// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_vma_manager.h`

use core::{
    ffi::c_void,
    mem::{align_of, size_of},
};

use drm_kapi::types::{DrmMm, DrmMmNode, RbNode, RbRoot, Rwlock};

use crate::drm_file_types::DrmFile;

#[repr(C, align(8))]
pub struct DrmVmaOffsetFile {
    pub vm_rb: RbNode,
    pub vm_tag: *mut DrmFile,
    pub vm_count: u64,
}

#[repr(C, align(8))]
pub struct DrmVmaOffsetNode {
    pub vm_lock: Rwlock,
    pub vm_node: DrmMmNode,
    pub vm_files: RbRoot,
    pub driver_private: *mut c_void,
}

#[repr(C, align(8))]
pub struct DrmVmaOffsetManager {
    pub vm_lock: Rwlock,
    pub vm_addr_space_mm: DrmMm,
}

const _: () = assert!(size_of::<DrmVmaOffsetFile>() == 40);
const _: () = assert!(align_of::<DrmVmaOffsetFile>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetFile, vm_rb) == 0);
const _: () = assert!(size_of::<RbNode>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetFile, vm_tag) == 24);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetFile, vm_count) == 32);
const _: () = assert!(size_of::<DrmVmaOffsetNode>() == 192);
const _: () = assert!(align_of::<DrmVmaOffsetNode>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetNode, vm_lock) == 0);
const _: () = assert!(size_of::<Rwlock>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetNode, vm_node) == 8);
const _: () = assert!(size_of::<DrmMmNode>() == 168);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetNode, vm_files) == 176);
const _: () = assert!(size_of::<RbRoot>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetNode, driver_private) == 184);
const _: () = assert!(size_of::<DrmVmaOffsetManager>() == 248);
const _: () = assert!(align_of::<DrmVmaOffsetManager>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetManager, vm_lock) == 0);
const _: () = assert!(size_of::<Rwlock>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVmaOffsetManager, vm_addr_space_mm) == 8);
const _: () = assert!(size_of::<DrmMm>() == 240);
