// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_modeset_lock.h`

use core::mem::{align_of, size_of};

use drm_kapi::{
    types::{ListHead, WwAcquireCtx},
    ww_mutex::WwMutex,
};

#[repr(C)]
pub struct DrmModesetAcquireCtx {
    pub ww_ctx: WwAcquireCtx,
    pub contended: *mut DrmModesetLock,
    pub stack_depot: u32,
    pub locked: ListHead,
    pub trylock_only: bool,
    pub interruptible: bool,
}

#[repr(C, align(8))]
pub struct DrmModesetLock {
    pub mutex: WwMutex,
    pub head: ListHead,
}

const _: () = assert!(size_of::<DrmModesetAcquireCtx>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmModesetAcquireCtx, ww_ctx) == 0);
const _: () = assert!(size_of::<WwAcquireCtx>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmModesetAcquireCtx, contended) == 24);
const _: () = assert!(core::mem::offset_of!(DrmModesetAcquireCtx, stack_depot) == 32);
const _: () = assert!(core::mem::offset_of!(DrmModesetAcquireCtx, locked) == 40);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModesetAcquireCtx, trylock_only) == 56);
const _: () = assert!(core::mem::offset_of!(DrmModesetAcquireCtx, interruptible) == 57);
const _: () = assert!(size_of::<DrmModesetLock>() == 48);
const _: () = assert!(align_of::<DrmModesetLock>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmModesetLock, mutex) == 0);
const _: () = assert!(size_of::<WwMutex>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmModesetLock, head) == 32);
const _: () = assert!(size_of::<ListHead>() == 16);
