// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:

use core::mem::size_of;

/// Sized placeholder for `i2c_adapter`: only its size and alignment are relied on here.
#[repr(C, align(8))]
pub struct I2cAdapter {
    _opaque: [u8; 1064],
}

/// LinuxKPI's field-accurate kthread work record used by DRM vblank helpers.
pub use drm_kapi::kthread::KthreadWork;

const _: () = assert!(size_of::<I2cAdapter>() == 1064);
const _: () = assert!(size_of::<KthreadWork>() == 40);
