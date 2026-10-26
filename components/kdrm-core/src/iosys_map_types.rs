// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/linux/iosys-map.h`

use core::{ffi::c_void, mem::size_of};

#[repr(C)]
pub union IosysMapAnon0 {
    pub vaddr_iomem: *mut c_void,
    pub vaddr: *mut c_void,
}

#[repr(C)]
pub struct IosysMap {
    pub _anon_0: IosysMapAnon0,
    pub is_iomem: bool,
}

const _: () = assert!(size_of::<IosysMapAnon0>() == 8);
const _: () = assert!(size_of::<IosysMap>() == 16);
const _: () = assert!(core::mem::offset_of!(IosysMap, _anon_0) == 0);
const _: () = assert!(core::mem::offset_of!(IosysMap, is_iomem) == 8);

/// Linux `iosys_map_set_vaddr()` (`include/linux/iosys-map.h`): record a
/// kernel-virtual address and clear `is_iomem`.
///
/// # Safety
/// `map` must be non-null, properly aligned, point to a valid live `IosysMap` instance,
/// and be valid for writes without conflicting concurrent accesses for the duration of the call.
#[inline]
pub unsafe fn iosys_map_set_vaddr(map: *mut IosysMap, vaddr: *mut c_void) {
    // SAFETY: The caller guarantees `map` is non-null, aligned, valid for writes,
    // and not aliased concurrently. Writing the union variant and boolean flag is within bounds.
    unsafe {
        (*map)._anon_0.vaddr = vaddr;
        (*map).is_iomem = false;
    }
}
