// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_vblank_work.h`

use core::mem::size_of;

use drm_kapi::types::ListHead;

use crate::{drm_vblank_types::DrmVblankCrtc, kernel_placeholder_types::KthreadWork};

#[repr(C)]
pub struct DrmVblankWork {
    pub base: KthreadWork,
    pub vblank: *mut DrmVblankCrtc,
    pub count: u64,
    pub cancelling: i32,
    pub node: ListHead,
}

const _: () = assert!(size_of::<DrmVblankWork>() == 80);
const _: () = assert!(core::mem::offset_of!(DrmVblankWork, base) == 0);
const _: () = assert!(size_of::<KthreadWork>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmVblankWork, vblank) == 40);
const _: () = assert!(core::mem::offset_of!(DrmVblankWork, count) == 48);
const _: () = assert!(core::mem::offset_of!(DrmVblankWork, cancelling) == 56);
const _: () = assert!(core::mem::offset_of!(DrmVblankWork, node) == 64);
const _: () = assert!(size_of::<ListHead>() == 16);
