// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_file.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
};

use drm_kapi::{
    completion::Completion,
    idr::Idr,
    types::{DmaFence, File, ListHead, Mutex, Spinlock, WaitQueueHead, XArray},
};

use crate::{
    drm_auth_types::DrmMaster, drm_device_types::DrmDevice, drm_prime_types::DrmPrimeFilePrivate,
    uapi_drm_types::DrmEvent,
};

#[repr(C)]
pub struct DrmMinor {
    pub index: i32,
    pub r#type: i32,
    pub kdev: *mut c_void,
    pub dev: *mut DrmDevice,
    pub debugfs_symlink: *mut c_void,
    pub debugfs_root: *mut c_void,
}

#[repr(C)]
pub struct DrmPendingEvent {
    pub completion: *mut Completion,
    pub completion_release: Option<unsafe extern "C" fn(*mut Completion)>,
    pub event: *mut DrmEvent,
    pub fence: *mut DmaFence,
    pub file_priv: *mut DrmFile,
    pub link: ListHead,
    pub pending_link: ListHead,
}

#[repr(C, align(8))]
pub struct DrmFile {
    pub authenticated: bool,
    pub stereo_allowed: bool,
    pub universal_planes: bool,
    pub atomic: bool,
    pub aspect_ratio_allowed: bool,
    pub writeback_connectors: bool,
    pub plane_color_pipeline: bool,
    pub was_master: bool,
    pub is_master: bool,
    pub supports_virtualized_cursor_plane: bool,
    pub master: *mut DrmMaster,
    pub master_lookup_lock: Spinlock,
    pub pid: *mut c_void,
    pub client_id: u64,
    pub magic: u32,
    pub lhead: ListHead,
    pub minor: *mut DrmMinor,
    pub object_idr: Idr,
    pub table_lock: Spinlock,
    pub syncobj_xa: XArray,
    pub filp: *mut File,
    pub driver_priv: *mut c_void,
    pub fbs: ListHead,
    pub fbs_lock: Mutex,
    pub blobs: ListHead,
    pub event_wait: WaitQueueHead,
    pub pending_event_list: ListHead,
    pub event_list: ListHead,
    pub event_space: i32,
    pub event_read_lock: Mutex,
    pub prime: DrmPrimeFilePrivate,
    pub client_name: *mut c_char,
    pub client_name_lock: Mutex,
    pub debugfs_client: *mut c_void,
}

#[repr(C)]
pub struct DrmMemoryStats {
    pub shared: u64,
    pub private: u64,
    pub resident: u64,
    pub purgeable: u64,
    pub active: u64,
}

const _: () = assert!(size_of::<DrmMinor>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmMinor, index) == 0);
const _: () = assert!(core::mem::offset_of!(DrmMinor, r#type) == 4);
const _: () = assert!(core::mem::offset_of!(DrmMinor, kdev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmMinor, dev) == 16);
const _: () = assert!(core::mem::offset_of!(DrmMinor, debugfs_symlink) == 24);
const _: () = assert!(core::mem::offset_of!(DrmMinor, debugfs_root) == 32);
const _: () = assert!(size_of::<DrmPendingEvent>() == 72);
const _: () = assert!(core::mem::offset_of!(DrmPendingEvent, completion) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPendingEvent, completion_release) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPendingEvent, event) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPendingEvent, fence) == 24);
const _: () = assert!(core::mem::offset_of!(DrmPendingEvent, file_priv) == 32);
const _: () = assert!(core::mem::offset_of!(DrmPendingEvent, link) == 40);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPendingEvent, pending_link) == 56);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(size_of::<DrmFile>() == 368);
const _: () = assert!(align_of::<DrmFile>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmFile, authenticated) == 0);
const _: () = assert!(core::mem::offset_of!(DrmFile, stereo_allowed) == 1);
const _: () = assert!(core::mem::offset_of!(DrmFile, universal_planes) == 2);
const _: () = assert!(core::mem::offset_of!(DrmFile, atomic) == 3);
const _: () = assert!(core::mem::offset_of!(DrmFile, aspect_ratio_allowed) == 4);
const _: () = assert!(core::mem::offset_of!(DrmFile, writeback_connectors) == 5);
const _: () = assert!(core::mem::offset_of!(DrmFile, plane_color_pipeline) == 6);
const _: () = assert!(core::mem::offset_of!(DrmFile, was_master) == 7);
const _: () = assert!(core::mem::offset_of!(DrmFile, is_master) == 8);
const _: () = assert!(core::mem::offset_of!(DrmFile, supports_virtualized_cursor_plane) == 9);
const _: () = assert!(core::mem::offset_of!(DrmFile, master) == 16);
const _: () = assert!(core::mem::offset_of!(DrmFile, master_lookup_lock) == 24);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmFile, pid) == 32);
const _: () = assert!(core::mem::offset_of!(DrmFile, client_id) == 40);
const _: () = assert!(core::mem::offset_of!(DrmFile, magic) == 48);
const _: () = assert!(core::mem::offset_of!(DrmFile, lhead) == 56);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFile, minor) == 72);
const _: () = assert!(core::mem::offset_of!(DrmFile, object_idr) == 80);
const _: () = assert!(size_of::<Idr>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFile, table_lock) == 104);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmFile, syncobj_xa) == 112);
const _: () = assert!(size_of::<XArray>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFile, filp) == 128);
const _: () = assert!(core::mem::offset_of!(DrmFile, driver_priv) == 136);
const _: () = assert!(core::mem::offset_of!(DrmFile, fbs) == 144);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFile, fbs_lock) == 160);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFile, blobs) == 184);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFile, event_wait) == 200);
const _: () = assert!(size_of::<WaitQueueHead>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFile, pending_event_list) == 224);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFile, event_list) == 240);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmFile, event_space) == 256);
const _: () = assert!(core::mem::offset_of!(DrmFile, event_read_lock) == 264);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFile, prime) == 288);
const _: () = assert!(size_of::<DrmPrimeFilePrivate>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmFile, client_name) == 328);
const _: () = assert!(core::mem::offset_of!(DrmFile, client_name_lock) == 336);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmFile, debugfs_client) == 360);
const _: () = assert!(size_of::<DrmMemoryStats>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmMemoryStats, shared) == 0);
const _: () = assert!(core::mem::offset_of!(DrmMemoryStats, private) == 8);
const _: () = assert!(core::mem::offset_of!(DrmMemoryStats, resident) == 16);
const _: () = assert!(core::mem::offset_of!(DrmMemoryStats, purgeable) == 24);
const _: () = assert!(core::mem::offset_of!(DrmMemoryStats, active) == 32);
