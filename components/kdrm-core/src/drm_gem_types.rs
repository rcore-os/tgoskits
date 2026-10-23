// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_gem.h`

use core::{
    ffi::c_void,
    mem::{align_of, size_of},
};

use drm_kapi::types::{File, Kref, ListHead, Mutex, SgTable};

use crate::{
    dma_resv_types::DmaResv, drm_device_types::DrmDevice, drm_file_types::DrmFile,
    drm_print_types::DrmPrinter, drm_vma_manager_types::DrmVmaOffsetNode,
    iosys_map_types::IosysMap,
};

#[repr(C)]
pub struct DrmGemObjectFuncs {
    pub free: Option<unsafe extern "C" fn(*mut DrmGemObject)>,
    pub open: Option<unsafe extern "C" fn(*mut DrmGemObject, *mut DrmFile) -> i32>,
    pub close: Option<unsafe extern "C" fn(*mut DrmGemObject, *mut DrmFile)>,
    pub print_info: Option<unsafe extern "C" fn(*mut DrmPrinter, u32, *mut DrmGemObject)>,
    pub export: Option<unsafe extern "C" fn(*mut DrmGemObject, i32) -> *mut c_void>,
    pub pin: Option<unsafe extern "C" fn(*mut DrmGemObject) -> i32>,
    pub unpin: Option<unsafe extern "C" fn(*mut DrmGemObject)>,
    pub get_sg_table: Option<unsafe extern "C" fn(*mut DrmGemObject) -> *mut SgTable>,
    pub vmap: Option<unsafe extern "C" fn(*mut DrmGemObject, *mut IosysMap) -> i32>,
    pub vunmap: Option<unsafe extern "C" fn(*mut DrmGemObject, *mut IosysMap)>,
    pub mmap: Option<unsafe extern "C" fn(*mut DrmGemObject, *mut c_void) -> i32>,
    pub evict: Option<unsafe extern "C" fn(*mut DrmGemObject) -> i32>,
    pub status: Option<unsafe extern "C" fn(*mut DrmGemObject) -> DrmGemObjectStatus>,
    pub rss: Option<unsafe extern "C" fn(*mut DrmGemObject) -> usize>,
    pub vm_ops: *mut c_void,
}

#[repr(C)]
pub struct DrmGemLru {
    pub count: i64,
    pub list: ListHead,
}

#[repr(C, align(8))]
pub struct DrmGemObjectGpuva {
    pub list: ListHead,
    pub lock: Mutex,
}

#[repr(C, align(8))]
pub struct DrmGemObject {
    pub refcount: Kref,
    pub handle_count: u32,
    pub dev: *mut DrmDevice,
    pub filp: *mut File,
    pub vma_node: DrmVmaOffsetNode,
    pub size: usize,
    pub name: i32,
    pub dma_buf: *mut c_void,
    pub import_attach: *mut c_void,
    pub resv: *mut DmaResv,
    pub _resv: DmaResv,
    pub gpuva: DrmGemObjectGpuva,
    pub funcs: *mut DrmGemObjectFuncs,
    pub lru_node: ListHead,
    pub lru: *mut DrmGemLru,
}

pub type DrmGemObjectStatus = u32;
pub const DRM_GEM_OBJECT_RESIDENT: DrmGemObjectStatus = 1;
pub const DRM_GEM_OBJECT_PURGEABLE: DrmGemObjectStatus = 2;
pub const DRM_GEM_OBJECT_ACTIVE: DrmGemObjectStatus = 4;

const _: () = assert!(size_of::<DrmGemObjectFuncs>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, free) == 0);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, open) == 8);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, close) == 16);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, print_info) == 24);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, export) == 32);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, pin) == 40);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, unpin) == 48);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, get_sg_table) == 56);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, vmap) == 64);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, vunmap) == 72);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, mmap) == 80);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, evict) == 88);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, status) == 96);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, rss) == 104);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectFuncs, vm_ops) == 112);
const _: () = assert!(size_of::<DrmGemLru>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmGemLru, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmGemLru, list) == 8);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(size_of::<DrmGemObjectGpuva>() == 40);
const _: () = assert!(align_of::<DrmGemObjectGpuva>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectGpuva, list) == 0);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmGemObjectGpuva, lock) == 16);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(size_of::<DrmGemObject>() == 368);
const _: () = assert!(align_of::<DrmGemObject>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, refcount) == 0);
const _: () = assert!(size_of::<Kref>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, handle_count) == 4);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, dev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, filp) == 16);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, vma_node) == 24);
const _: () = assert!(size_of::<DrmVmaOffsetNode>() == 192);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, size) == 216);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, name) == 224);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, dma_buf) == 232);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, import_attach) == 240);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, resv) == 248);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, _resv) == 256);
const _: () = assert!(size_of::<DmaResv>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, gpuva) == 296);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, funcs) == 336);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, lru_node) == 344);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmGemObject, lru) == 360);
