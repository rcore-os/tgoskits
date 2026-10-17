// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_mm.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::mem::size_of;

use drm_kapi::types::DrmMm;

#[repr(C)]
pub struct DrmMmScan {
    pub mm: *mut DrmMm,
    pub size: u64,
    pub alignment: u64,
    pub remainder_mask: u64,
    pub range_start: u64,
    pub range_end: u64,
    pub hit_start: u64,
    pub hit_end: u64,
    pub color: u64,
    pub mode: DrmMmInsertMode,
}

pub type DrmMmInsertMode = u32;
pub const DRM_MM_INSERT_BEST: DrmMmInsertMode = 0;
pub const DRM_MM_INSERT_LOW: DrmMmInsertMode = 1;
pub const DRM_MM_INSERT_HIGH: DrmMmInsertMode = 2;
pub const DRM_MM_INSERT_EVICT: DrmMmInsertMode = 3;
pub const DRM_MM_INSERT_ONCE: DrmMmInsertMode = 2147483648;
pub const DRM_MM_INSERT_HIGHEST: DrmMmInsertMode = 2147483650;
pub const DRM_MM_INSERT_LOWEST: DrmMmInsertMode = 2147483649;

const _: () = assert!(size_of::<DrmMmScan>() == 80);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, mm) == 0);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, size) == 8);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, alignment) == 16);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, remainder_mask) == 24);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, range_start) == 32);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, range_end) == 40);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, hit_start) == 48);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, hit_end) == 56);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, color) == 64);
const _: () = assert!(core::mem::offset_of!(DrmMmScan, mode) == 72);
const _: () = assert!(size_of::<DrmMmInsertMode>() == 4);
