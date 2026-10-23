// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_damage_helper.h`

use core::mem::size_of;

use crate::drm_rect_types::DrmRect;

#[repr(C)]
pub struct DrmAtomicHelperDamageIter {
    pub plane_src: DrmRect,
    pub clips: *mut DrmRect,
    pub num_clips: u32,
    pub curr_clip: u32,
    pub full_update: bool,
}

const _: () = assert!(size_of::<DrmAtomicHelperDamageIter>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmAtomicHelperDamageIter, plane_src) == 0);
const _: () = assert!(size_of::<DrmRect>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmAtomicHelperDamageIter, clips) == 16);
const _: () = assert!(core::mem::offset_of!(DrmAtomicHelperDamageIter, num_clips) == 24);
const _: () = assert!(core::mem::offset_of!(DrmAtomicHelperDamageIter, curr_clip) == 28);
const _: () = assert!(core::mem::offset_of!(DrmAtomicHelperDamageIter, full_update) == 32);
