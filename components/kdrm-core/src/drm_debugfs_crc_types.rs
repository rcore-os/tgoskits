// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_debugfs_crc.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::c_char,
    mem::{align_of, size_of},
};

use drm_kapi::types::{Spinlock, WaitQueueHead};

#[repr(C)]
pub struct DrmCrtcCrcEntry {
    pub has_frame_counter: bool,
    pub frame: u32,
    pub crcs: [u32; 10],
}

#[repr(C, align(8))]
pub struct DrmCrtcCrc {
    pub lock: Spinlock,
    pub source: *mut c_char,
    pub opened: bool,
    pub overflow: bool,
    pub entries: *mut DrmCrtcCrcEntry,
    pub head: i32,
    pub tail: i32,
    pub values_cnt: usize,
    pub wq: WaitQueueHead,
}

const _: () = assert!(size_of::<DrmCrtcCrcEntry>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrcEntry, has_frame_counter) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrcEntry, frame) == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrcEntry, crcs) == 8);
const _: () = assert!(size_of::<[u32; 10]>() == 40);
const _: () = assert!(size_of::<DrmCrtcCrc>() == 72);
const _: () = assert!(align_of::<DrmCrtcCrc>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, lock) == 0);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, source) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, opened) == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, overflow) == 17);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, entries) == 24);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, head) == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, tail) == 36);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, values_cnt) == 40);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCrc, wq) == 48);
const _: () = assert!(size_of::<WaitQueueHead>() == 24);
