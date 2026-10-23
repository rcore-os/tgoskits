// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_prime.h`

use core::mem::{align_of, size_of};

use drm_kapi::types::{Mutex, RbRoot};

#[repr(C, align(8))]
pub struct DrmPrimeFilePrivate {
    pub lock: Mutex,
    pub dmabufs: RbRoot,
    pub handles: RbRoot,
}

const _: () = assert!(size_of::<DrmPrimeFilePrivate>() == 40);
const _: () = assert!(align_of::<DrmPrimeFilePrivate>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrimeFilePrivate, lock) == 0);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmPrimeFilePrivate, dmabufs) == 24);
const _: () = assert!(size_of::<RbRoot>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrimeFilePrivate, handles) == 32);
const _: () = assert!(size_of::<RbRoot>() == 8);
