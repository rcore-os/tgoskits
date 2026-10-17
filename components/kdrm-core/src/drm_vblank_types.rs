// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_vblank.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::c_void,
    mem::{ManuallyDrop, align_of, size_of},
};

use drm_kapi::types::{
    AtomicT, AtomicT64, Hrtimer, ListHead, Seqlock, Spinlock, TimerList, WaitQueueHead,
};

use crate::{
    drm_crtc_types::DrmCrtc,
    drm_device_types::DrmDevice,
    drm_file_types::DrmPendingEvent,
    drm_modes_types::DrmDisplayMode,
    uapi_drm_types::{DrmEvent, DrmEventCrtcSequence, DrmEventVblank},
};

#[repr(C)]
pub union DrmPendingVblankEventEvent {
    pub base: ManuallyDrop<DrmEvent>,
    pub vbl: ManuallyDrop<DrmEventVblank>,
    pub seq: ManuallyDrop<DrmEventCrtcSequence>,
}

#[repr(C)]
pub struct DrmPendingVblankEvent {
    pub base: DrmPendingEvent,
    pub pipe: u32,
    pub sequence: u64,
    pub event: DrmPendingVblankEventEvent,
}

#[repr(C)]
pub struct DrmVblankCrtcConfig {
    pub offdelay_ms: i32,
    pub disable_immediate: bool,
}

#[repr(C, align(8))]
pub struct DrmVblankCrtcTimer {
    pub timer: Hrtimer,
    pub interval_lock: Spinlock,
    pub interval: i64,
    pub crtc: *mut DrmCrtc,
}

#[repr(C, align(8))]
pub struct DrmVblankCrtc {
    pub dev: *mut DrmDevice,
    pub queue: WaitQueueHead,
    pub disable_timer: TimerList,
    pub seqlock: Seqlock,
    pub count: AtomicT64,
    pub time: i64,
    pub refcount: AtomicT,
    pub last: u32,
    pub max_vblank_count: u32,
    pub inmodeset: u32,
    pub pipe: u32,
    pub framedur_ns: i32,
    pub linedur_ns: i32,
    pub hwmode: DrmDisplayMode,
    pub config: DrmVblankCrtcConfig,
    pub enabled: bool,
    pub worker: *mut c_void,
    pub pending_work: ListHead,
    pub work_wait_queue: WaitQueueHead,
    pub vblank_timer: DrmVblankCrtcTimer,
}

const _: () = assert!(size_of::<DrmPendingVblankEventEvent>() == 32);
const _: () = assert!(size_of::<DrmPendingVblankEvent>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmPendingVblankEvent, base) == 0);
const _: () = assert!(size_of::<DrmPendingEvent>() == 72);
const _: () = assert!(core::mem::offset_of!(DrmPendingVblankEvent, pipe) == 72);
const _: () = assert!(core::mem::offset_of!(DrmPendingVblankEvent, sequence) == 80);
const _: () = assert!(core::mem::offset_of!(DrmPendingVblankEvent, event) == 88);
const _: () = assert!(size_of::<DrmVblankCrtcConfig>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtcConfig, offdelay_ms) == 0);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtcConfig, disable_immediate) == 4);
const _: () = assert!(size_of::<DrmVblankCrtcTimer>() == 104);
const _: () = assert!(align_of::<DrmVblankCrtcTimer>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtcTimer, timer) == 0);
const _: () = assert!(size_of::<Hrtimer>() == 80);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtcTimer, interval_lock) == 80);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtcTimer, interval) == 88);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtcTimer, crtc) == 96);
const _: () = assert!(size_of::<DrmVblankCrtc>() == 416);
const _: () = assert!(align_of::<DrmVblankCrtc>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, queue) == 8);
const _: () = assert!(size_of::<WaitQueueHead>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, disable_timer) == 32);
const _: () = assert!(size_of::<TimerList>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, seqlock) == 72);
const _: () = assert!(size_of::<Seqlock>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, count) == 80);
const _: () = assert!(size_of::<AtomicT64>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, time) == 88);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, refcount) == 96);
const _: () = assert!(size_of::<AtomicT>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, last) == 100);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, max_vblank_count) == 104);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, inmodeset) == 108);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, pipe) == 112);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, framedur_ns) == 116);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, linedur_ns) == 120);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, hwmode) == 128);
const _: () = assert!(size_of::<DrmDisplayMode>() == 120);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, config) == 248);
const _: () = assert!(size_of::<DrmVblankCrtcConfig>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, enabled) == 256);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, worker) == 264);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, pending_work) == 272);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, work_wait_queue) == 288);
const _: () = assert!(size_of::<WaitQueueHead>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmVblankCrtc, vblank_timer) == 312);
const _: () = assert!(size_of::<DrmVblankCrtcTimer>() == 104);
