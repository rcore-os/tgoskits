// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/uapi/drm/drm.h`

use core::{
    ffi::{c_char, c_void},
    mem::{ManuallyDrop, size_of},
};

#[repr(C)]
pub struct DrmClipRect {
    pub x1: u16,
    pub y1: u16,
    pub x2: u16,
    pub y2: u16,
}

#[repr(C)]
pub struct DrmDrawableInfo {
    pub num_rects: u32,
    pub rects: *mut DrmClipRect,
}

#[repr(C)]
pub struct DrmTexRegion {
    pub next: u8,
    pub prev: u8,
    pub in_use: u8,
    pub padding: u8,
    pub age: u32,
}

#[repr(C)]
pub struct DrmHwLock {
    pub lock: u32,
    pub padding: [c_char; 60],
}

#[repr(C)]
pub struct DrmVersion {
    pub version_major: i32,
    pub version_minor: i32,
    pub version_patchlevel: i32,
    pub name_len: usize,
    pub name: *mut c_char,
    pub date_len: usize,
    pub date: *mut c_char,
    pub desc_len: usize,
    pub desc: *mut c_char,
}

#[repr(C)]
pub struct DrmUnique {
    pub unique_len: usize,
    pub unique: *mut c_char,
}

#[repr(C)]
pub struct DrmList {
    pub count: i32,
    pub version: *mut DrmVersion,
}

#[repr(C)]
pub struct DrmBlock {
    pub unused: i32,
}

#[repr(C)]
pub struct DrmControl {
    pub func: u32,
    pub irq: i32,
}

#[repr(C)]
pub struct DrmCtxPrivMap {
    pub ctx_id: u32,
    pub handle: *mut c_void,
}

#[repr(C)]
pub struct DrmMap {
    pub offset: u64,
    pub size: u64,
    pub r#type: DrmMapType,
    pub flags: DrmMapFlags,
    pub handle: *mut c_void,
    pub mtrr: i32,
}

#[repr(C)]
pub struct DrmClient {
    pub idx: i32,
    pub auth: i32,
    pub pid: u64,
    pub uid: u64,
    pub magic: u64,
    pub iocs: u64,
}

#[repr(C)]
pub struct DrmStatsData {
    pub value: u64,
    pub r#type: DrmStatType,
}

#[repr(C)]
pub struct DrmStats {
    pub count: u64,
    pub data: [DrmStatsData; 15],
}

#[repr(C)]
pub struct DrmLock {
    pub context: i32,
    pub flags: DrmLockFlags,
}

#[repr(C)]
pub struct DrmBufDesc {
    pub count: i32,
    pub size: i32,
    pub low_mark: i32,
    pub high_mark: i32,
    pub flags: u32,
    pub agp_start: u64,
}

#[repr(C)]
pub struct DrmBufInfo {
    pub count: i32,
    pub list: *mut DrmBufDesc,
}

#[repr(C)]
pub struct DrmBufFree {
    pub count: i32,
    pub list: *mut i32,
}

#[repr(C)]
pub struct DrmBufPub {
    pub idx: i32,
    pub total: i32,
    pub used: i32,
    pub address: *mut c_void,
}

#[repr(C)]
pub struct DrmBufMap {
    pub count: i32,
    pub r#virtual: *mut c_void,
    pub list: *mut DrmBufPub,
}

#[repr(C)]
pub struct DrmDma {
    pub context: i32,
    pub send_count: i32,
    pub send_indices: *mut i32,
    pub send_sizes: *mut i32,
    pub flags: DrmDmaFlags,
    pub request_count: i32,
    pub request_size: i32,
    pub request_indices: *mut i32,
    pub request_sizes: *mut i32,
    pub granted_count: i32,
}

#[repr(C)]
pub struct DrmCtx {
    pub handle: u32,
    pub flags: DrmCtxFlags,
}

#[repr(C)]
pub struct DrmCtxRes {
    pub count: i32,
    pub contexts: *mut DrmCtx,
}

#[repr(C)]
pub struct DrmDraw {
    pub handle: u32,
}

#[repr(C)]
pub struct DrmUpdateDraw {
    pub handle: u32,
    pub r#type: u32,
    pub num: u32,
    pub data: u64,
}

#[repr(C)]
pub struct DrmAuth {
    pub magic: u32,
}

#[repr(C)]
pub struct DrmIrqBusid {
    pub irq: i32,
    pub busnum: i32,
    pub devnum: i32,
    pub funcnum: i32,
}

#[repr(C)]
pub struct DrmWaitVblankRequest {
    pub r#type: DrmVblankSeqType,
    pub sequence: u32,
    pub signal: u64,
}

#[repr(C)]
pub struct DrmWaitVblankReply {
    pub r#type: DrmVblankSeqType,
    pub sequence: u32,
    pub tval_sec: i64,
    pub tval_usec: i64,
}

#[repr(C)]
pub union DrmWaitVblank {
    pub request: ManuallyDrop<DrmWaitVblankRequest>,
    pub reply: ManuallyDrop<DrmWaitVblankReply>,
}

#[repr(C)]
pub struct DrmModesetCtl {
    pub crtc: u32,
    pub cmd: u32,
}

#[repr(C)]
pub struct DrmAgpMode {
    pub mode: u64,
}

#[repr(C)]
pub struct DrmAgpBuffer {
    pub size: u64,
    pub handle: u64,
    pub r#type: u64,
    pub physical: u64,
}

#[repr(C)]
pub struct DrmAgpBinding {
    pub handle: u64,
    pub offset: u64,
}

#[repr(C)]
pub struct DrmAgpInfo {
    pub agp_version_major: i32,
    pub agp_version_minor: i32,
    pub mode: u64,
    pub aperture_base: u64,
    pub aperture_size: u64,
    pub memory_allowed: u64,
    pub memory_used: u64,
    pub id_vendor: u16,
    pub id_device: u16,
}

#[repr(C)]
pub struct DrmScatterGather {
    pub size: u64,
    pub handle: u64,
}

#[repr(C)]
pub struct DrmSetVersion {
    pub drm_di_major: i32,
    pub drm_di_minor: i32,
    pub drm_dd_major: i32,
    pub drm_dd_minor: i32,
}

#[repr(C)]
pub struct DrmGemClose {
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
pub struct DrmGemFlink {
    pub handle: u32,
    pub name: u32,
}

#[repr(C)]
pub struct DrmGemOpen {
    pub name: u32,
    pub handle: u32,
    pub size: u64,
}

#[repr(C)]
pub struct DrmGemChangeHandle {
    pub handle: u32,
    pub new_handle: u32,
}

#[repr(C)]
pub struct DrmGetCap {
    pub capability: u64,
    pub value: u64,
}

#[repr(C)]
pub struct DrmSetClientCap {
    pub capability: u64,
    pub value: u64,
}

#[repr(C)]
pub struct DrmPrimeHandle {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
}

#[repr(C)]
pub struct DrmSyncobjCreate {
    pub handle: u32,
    pub flags: u32,
}

#[repr(C)]
pub struct DrmSyncobjDestroy {
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
pub struct DrmSyncobjHandle {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
    pub pad: u32,
    pub point: u64,
}

#[repr(C)]
pub struct DrmSyncobjTransfer {
    pub src_handle: u32,
    pub dst_handle: u32,
    pub src_point: u64,
    pub dst_point: u64,
    pub flags: u32,
    pub pad: u32,
}

#[repr(C)]
pub struct DrmSyncobjWait {
    pub handles: u64,
    pub timeout_nsec: i64,
    pub count_handles: u32,
    pub flags: u32,
    pub first_signaled: u32,
    pub pad: u32,
    pub deadline_nsec: u64,
}

#[repr(C)]
pub struct DrmSyncobjTimelineWait {
    pub handles: u64,
    pub points: u64,
    pub timeout_nsec: i64,
    pub count_handles: u32,
    pub flags: u32,
    pub first_signaled: u32,
    pub pad: u32,
    pub deadline_nsec: u64,
}

#[repr(C)]
pub struct DrmSyncobjEventfd {
    pub handle: u32,
    pub flags: u32,
    pub point: u64,
    pub fd: i32,
    pub pad: u32,
}

#[repr(C)]
pub struct DrmSyncobjArray {
    pub handles: u64,
    pub count_handles: u32,
    pub pad: u32,
}

#[repr(C)]
pub struct DrmSyncobjTimelineArray {
    pub handles: u64,
    pub points: u64,
    pub count_handles: u32,
    pub flags: u32,
}

#[repr(C)]
pub struct DrmCrtcGetSequence {
    pub crtc_id: u32,
    pub active: u32,
    pub sequence: u64,
    pub sequence_ns: i64,
}

#[repr(C)]
pub struct DrmCrtcQueueSequence {
    pub crtc_id: u32,
    pub flags: u32,
    pub sequence: u64,
    pub user_data: u64,
}

#[repr(C)]
pub struct DrmSetClientName {
    pub name_len: u64,
    pub name: u64,
}

#[repr(C)]
pub struct DrmEvent {
    pub r#type: u32,
    pub length: u32,
}

#[repr(C)]
pub struct DrmEventVblank {
    pub base: DrmEvent,
    pub user_data: u64,
    pub tv_sec: u32,
    pub tv_usec: u32,
    pub sequence: u32,
    pub crtc_id: u32,
}

#[repr(C)]
pub struct DrmEventCrtcSequence {
    pub base: DrmEvent,
    pub user_data: u64,
    pub time_ns: i64,
    pub sequence: u64,
}

pub type DrmMapType = u32;
pub const _DRM_FRAME_BUFFER: DrmMapType = 0;
pub const _DRM_REGISTERS: DrmMapType = 1;
pub const _DRM_SHM: DrmMapType = 2;
pub const _DRM_AGP: DrmMapType = 3;
pub const _DRM_SCATTER_GATHER: DrmMapType = 4;
pub const _DRM_CONSISTENT: DrmMapType = 5;

pub type DrmMapFlags = u32;
pub const _DRM_RESTRICTED: DrmMapFlags = 1;
pub const _DRM_READ_ONLY: DrmMapFlags = 2;
pub const _DRM_LOCKED: DrmMapFlags = 4;
pub const _DRM_KERNEL: DrmMapFlags = 8;
pub const _DRM_WRITE_COMBINING: DrmMapFlags = 16;
pub const _DRM_CONTAINS_LOCK: DrmMapFlags = 32;
pub const _DRM_REMOVABLE: DrmMapFlags = 64;
pub const _DRM_DRIVER: DrmMapFlags = 128;

pub type DrmStatType = u32;
pub const _DRM_STAT_LOCK: DrmStatType = 0;
pub const _DRM_STAT_OPENS: DrmStatType = 1;
pub const _DRM_STAT_CLOSES: DrmStatType = 2;
pub const _DRM_STAT_IOCTLS: DrmStatType = 3;
pub const _DRM_STAT_LOCKS: DrmStatType = 4;
pub const _DRM_STAT_UNLOCKS: DrmStatType = 5;
pub const _DRM_STAT_VALUE: DrmStatType = 6;
pub const _DRM_STAT_BYTE: DrmStatType = 7;
pub const _DRM_STAT_COUNT: DrmStatType = 8;
pub const _DRM_STAT_IRQ: DrmStatType = 9;
pub const _DRM_STAT_PRIMARY: DrmStatType = 10;
pub const _DRM_STAT_SECONDARY: DrmStatType = 11;
pub const _DRM_STAT_DMA: DrmStatType = 12;
pub const _DRM_STAT_SPECIAL: DrmStatType = 13;
pub const _DRM_STAT_MISSED: DrmStatType = 14;

pub type DrmLockFlags = u32;
pub const _DRM_LOCK_READY: DrmLockFlags = 1;
pub const _DRM_LOCK_QUIESCENT: DrmLockFlags = 2;
pub const _DRM_LOCK_FLUSH: DrmLockFlags = 4;
pub const _DRM_LOCK_FLUSH_ALL: DrmLockFlags = 8;
pub const _DRM_HALT_ALL_QUEUES: DrmLockFlags = 16;
pub const _DRM_HALT_CUR_QUEUES: DrmLockFlags = 32;

pub type DrmDmaFlags = u32;
pub const _DRM_DMA_BLOCK: DrmDmaFlags = 1;
pub const _DRM_DMA_WHILE_LOCKED: DrmDmaFlags = 2;
pub const _DRM_DMA_PRIORITY: DrmDmaFlags = 4;
pub const _DRM_DMA_WAIT: DrmDmaFlags = 16;
pub const _DRM_DMA_SMALLER_OK: DrmDmaFlags = 32;
pub const _DRM_DMA_LARGER_OK: DrmDmaFlags = 64;

pub type DrmCtxFlags = u32;
pub const _DRM_CONTEXT_PRESERVED: DrmCtxFlags = 1;
pub const _DRM_CONTEXT_2DONLY: DrmCtxFlags = 2;

pub type DrmVblankSeqType = u32;
pub const _DRM_VBLANK_ABSOLUTE: DrmVblankSeqType = 0;
pub const _DRM_VBLANK_RELATIVE: DrmVblankSeqType = 1;
pub const _DRM_VBLANK_HIGH_CRTC_MASK: DrmVblankSeqType = 62;
pub const _DRM_VBLANK_EVENT: DrmVblankSeqType = 67108864;
pub const _DRM_VBLANK_FLIP: DrmVblankSeqType = 134217728;
pub const _DRM_VBLANK_NEXTONMISS: DrmVblankSeqType = 268435456;
pub const _DRM_VBLANK_SECONDARY: DrmVblankSeqType = 536870912;
pub const _DRM_VBLANK_SIGNAL: DrmVblankSeqType = 1073741824;

const _: () = assert!(size_of::<DrmClipRect>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmClipRect, x1) == 0);
const _: () = assert!(core::mem::offset_of!(DrmClipRect, y1) == 2);
const _: () = assert!(core::mem::offset_of!(DrmClipRect, x2) == 4);
const _: () = assert!(core::mem::offset_of!(DrmClipRect, y2) == 6);
const _: () = assert!(size_of::<DrmDrawableInfo>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDrawableInfo, num_rects) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDrawableInfo, rects) == 8);
const _: () = assert!(size_of::<DrmTexRegion>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmTexRegion, next) == 0);
const _: () = assert!(core::mem::offset_of!(DrmTexRegion, prev) == 1);
const _: () = assert!(core::mem::offset_of!(DrmTexRegion, in_use) == 2);
const _: () = assert!(core::mem::offset_of!(DrmTexRegion, padding) == 3);
const _: () = assert!(core::mem::offset_of!(DrmTexRegion, age) == 4);
const _: () = assert!(size_of::<DrmHwLock>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmHwLock, lock) == 0);
const _: () = assert!(core::mem::offset_of!(DrmHwLock, padding) == 4);
const _: () = assert!(size_of::<[c_char; 60]>() == 60);
const _: () = assert!(size_of::<DrmVersion>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmVersion, version_major) == 0);
const _: () = assert!(core::mem::offset_of!(DrmVersion, version_minor) == 4);
const _: () = assert!(core::mem::offset_of!(DrmVersion, version_patchlevel) == 8);
const _: () = assert!(core::mem::offset_of!(DrmVersion, name_len) == 16);
const _: () = assert!(core::mem::offset_of!(DrmVersion, name) == 24);
const _: () = assert!(core::mem::offset_of!(DrmVersion, date_len) == 32);
const _: () = assert!(core::mem::offset_of!(DrmVersion, date) == 40);
const _: () = assert!(core::mem::offset_of!(DrmVersion, desc_len) == 48);
const _: () = assert!(core::mem::offset_of!(DrmVersion, desc) == 56);
const _: () = assert!(size_of::<DrmUnique>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmUnique, unique_len) == 0);
const _: () = assert!(core::mem::offset_of!(DrmUnique, unique) == 8);
const _: () = assert!(size_of::<DrmList>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmList, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmList, version) == 8);
const _: () = assert!(size_of::<DrmBlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmBlock, unused) == 0);
const _: () = assert!(size_of::<DrmControl>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmControl, func) == 0);
const _: () = assert!(core::mem::offset_of!(DrmControl, irq) == 4);
const _: () = assert!(size_of::<DrmCtxPrivMap>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmCtxPrivMap, ctx_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCtxPrivMap, handle) == 8);
const _: () = assert!(size_of::<DrmMap>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmMap, offset) == 0);
const _: () = assert!(core::mem::offset_of!(DrmMap, size) == 8);
const _: () = assert!(core::mem::offset_of!(DrmMap, r#type) == 16);
const _: () = assert!(size_of::<DrmMapType>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmMap, flags) == 20);
const _: () = assert!(size_of::<DrmMapFlags>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmMap, handle) == 24);
const _: () = assert!(core::mem::offset_of!(DrmMap, mtrr) == 32);
const _: () = assert!(size_of::<DrmClient>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmClient, idx) == 0);
const _: () = assert!(core::mem::offset_of!(DrmClient, auth) == 4);
const _: () = assert!(core::mem::offset_of!(DrmClient, pid) == 8);
const _: () = assert!(core::mem::offset_of!(DrmClient, uid) == 16);
const _: () = assert!(core::mem::offset_of!(DrmClient, magic) == 24);
const _: () = assert!(core::mem::offset_of!(DrmClient, iocs) == 32);
const _: () = assert!(size_of::<DrmStatsData>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmStatsData, value) == 0);
const _: () = assert!(core::mem::offset_of!(DrmStatsData, r#type) == 8);
const _: () = assert!(size_of::<DrmStatType>() == 4);
const _: () = assert!(size_of::<DrmStats>() == 248);
const _: () = assert!(core::mem::offset_of!(DrmStats, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmStats, data) == 8);
const _: () = assert!(size_of::<DrmLock>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmLock, context) == 0);
const _: () = assert!(core::mem::offset_of!(DrmLock, flags) == 4);
const _: () = assert!(size_of::<DrmLockFlags>() == 4);
const _: () = assert!(size_of::<DrmBufDesc>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmBufDesc, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmBufDesc, size) == 4);
const _: () = assert!(core::mem::offset_of!(DrmBufDesc, low_mark) == 8);
const _: () = assert!(core::mem::offset_of!(DrmBufDesc, high_mark) == 12);
const _: () = assert!(core::mem::offset_of!(DrmBufDesc, flags) == 16);
const _: () = assert!(core::mem::offset_of!(DrmBufDesc, agp_start) == 24);
const _: () = assert!(size_of::<DrmBufInfo>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmBufInfo, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmBufInfo, list) == 8);
const _: () = assert!(size_of::<DrmBufFree>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmBufFree, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmBufFree, list) == 8);
const _: () = assert!(size_of::<DrmBufPub>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmBufPub, idx) == 0);
const _: () = assert!(core::mem::offset_of!(DrmBufPub, total) == 4);
const _: () = assert!(core::mem::offset_of!(DrmBufPub, used) == 8);
const _: () = assert!(core::mem::offset_of!(DrmBufPub, address) == 16);
const _: () = assert!(size_of::<DrmBufMap>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmBufMap, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmBufMap, r#virtual) == 8);
const _: () = assert!(core::mem::offset_of!(DrmBufMap, list) == 16);
const _: () = assert!(size_of::<DrmDma>() == 64);
const _: () = assert!(core::mem::offset_of!(DrmDma, context) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDma, send_count) == 4);
const _: () = assert!(core::mem::offset_of!(DrmDma, send_indices) == 8);
const _: () = assert!(core::mem::offset_of!(DrmDma, send_sizes) == 16);
const _: () = assert!(core::mem::offset_of!(DrmDma, flags) == 24);
const _: () = assert!(size_of::<DrmDmaFlags>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDma, request_count) == 28);
const _: () = assert!(core::mem::offset_of!(DrmDma, request_size) == 32);
const _: () = assert!(core::mem::offset_of!(DrmDma, request_indices) == 40);
const _: () = assert!(core::mem::offset_of!(DrmDma, request_sizes) == 48);
const _: () = assert!(core::mem::offset_of!(DrmDma, granted_count) == 56);
const _: () = assert!(size_of::<DrmCtx>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmCtx, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCtx, flags) == 4);
const _: () = assert!(size_of::<DrmCtxFlags>() == 4);
const _: () = assert!(size_of::<DrmCtxRes>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmCtxRes, count) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCtxRes, contexts) == 8);
const _: () = assert!(size_of::<DrmDraw>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDraw, handle) == 0);
const _: () = assert!(size_of::<DrmUpdateDraw>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmUpdateDraw, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmUpdateDraw, r#type) == 4);
const _: () = assert!(core::mem::offset_of!(DrmUpdateDraw, num) == 8);
const _: () = assert!(core::mem::offset_of!(DrmUpdateDraw, data) == 16);
const _: () = assert!(size_of::<DrmAuth>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmAuth, magic) == 0);
const _: () = assert!(size_of::<DrmIrqBusid>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmIrqBusid, irq) == 0);
const _: () = assert!(core::mem::offset_of!(DrmIrqBusid, busnum) == 4);
const _: () = assert!(core::mem::offset_of!(DrmIrqBusid, devnum) == 8);
const _: () = assert!(core::mem::offset_of!(DrmIrqBusid, funcnum) == 12);
const _: () = assert!(size_of::<DrmWaitVblankRequest>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmWaitVblankRequest, r#type) == 0);
const _: () = assert!(size_of::<DrmVblankSeqType>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmWaitVblankRequest, sequence) == 4);
const _: () = assert!(core::mem::offset_of!(DrmWaitVblankRequest, signal) == 8);
const _: () = assert!(size_of::<DrmWaitVblankReply>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmWaitVblankReply, r#type) == 0);
const _: () = assert!(size_of::<DrmVblankSeqType>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmWaitVblankReply, sequence) == 4);
const _: () = assert!(core::mem::offset_of!(DrmWaitVblankReply, tval_sec) == 8);
const _: () = assert!(core::mem::offset_of!(DrmWaitVblankReply, tval_usec) == 16);
const _: () = assert!(size_of::<DrmWaitVblank>() == 24);
const _: () = assert!(size_of::<DrmModesetCtl>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmModesetCtl, crtc) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModesetCtl, cmd) == 4);
const _: () = assert!(size_of::<DrmAgpMode>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmAgpMode, mode) == 0);
const _: () = assert!(size_of::<DrmAgpBuffer>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmAgpBuffer, size) == 0);
const _: () = assert!(core::mem::offset_of!(DrmAgpBuffer, handle) == 8);
const _: () = assert!(core::mem::offset_of!(DrmAgpBuffer, r#type) == 16);
const _: () = assert!(core::mem::offset_of!(DrmAgpBuffer, physical) == 24);
const _: () = assert!(size_of::<DrmAgpBinding>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmAgpBinding, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmAgpBinding, offset) == 8);
const _: () = assert!(size_of::<DrmAgpInfo>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, agp_version_major) == 0);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, agp_version_minor) == 4);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, mode) == 8);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, aperture_base) == 16);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, aperture_size) == 24);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, memory_allowed) == 32);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, memory_used) == 40);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, id_vendor) == 48);
const _: () = assert!(core::mem::offset_of!(DrmAgpInfo, id_device) == 50);
const _: () = assert!(size_of::<DrmScatterGather>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmScatterGather, size) == 0);
const _: () = assert!(core::mem::offset_of!(DrmScatterGather, handle) == 8);
const _: () = assert!(size_of::<DrmSetVersion>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmSetVersion, drm_di_major) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSetVersion, drm_di_minor) == 4);
const _: () = assert!(core::mem::offset_of!(DrmSetVersion, drm_dd_major) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSetVersion, drm_dd_minor) == 12);
const _: () = assert!(size_of::<DrmGemClose>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmGemClose, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmGemClose, pad) == 4);
const _: () = assert!(size_of::<DrmGemFlink>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmGemFlink, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmGemFlink, name) == 4);
const _: () = assert!(size_of::<DrmGemOpen>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmGemOpen, name) == 0);
const _: () = assert!(core::mem::offset_of!(DrmGemOpen, handle) == 4);
const _: () = assert!(core::mem::offset_of!(DrmGemOpen, size) == 8);
const _: () = assert!(size_of::<DrmGemChangeHandle>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmGemChangeHandle, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmGemChangeHandle, new_handle) == 4);
const _: () = assert!(size_of::<DrmGetCap>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmGetCap, capability) == 0);
const _: () = assert!(core::mem::offset_of!(DrmGetCap, value) == 8);
const _: () = assert!(size_of::<DrmSetClientCap>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmSetClientCap, capability) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSetClientCap, value) == 8);
const _: () = assert!(size_of::<DrmPrimeHandle>() == 12);
const _: () = assert!(core::mem::offset_of!(DrmPrimeHandle, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrimeHandle, flags) == 4);
const _: () = assert!(core::mem::offset_of!(DrmPrimeHandle, fd) == 8);
const _: () = assert!(size_of::<DrmSyncobjCreate>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjCreate, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjCreate, flags) == 4);
const _: () = assert!(size_of::<DrmSyncobjDestroy>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjDestroy, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjDestroy, pad) == 4);
const _: () = assert!(size_of::<DrmSyncobjHandle>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjHandle, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjHandle, flags) == 4);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjHandle, fd) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjHandle, pad) == 12);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjHandle, point) == 16);
const _: () = assert!(size_of::<DrmSyncobjTransfer>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTransfer, src_handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTransfer, dst_handle) == 4);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTransfer, src_point) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTransfer, dst_point) == 16);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTransfer, flags) == 24);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTransfer, pad) == 28);
const _: () = assert!(size_of::<DrmSyncobjWait>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjWait, handles) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjWait, timeout_nsec) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjWait, count_handles) == 16);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjWait, flags) == 20);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjWait, first_signaled) == 24);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjWait, pad) == 28);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjWait, deadline_nsec) == 32);
const _: () = assert!(size_of::<DrmSyncobjTimelineWait>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, handles) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, points) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, timeout_nsec) == 16);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, count_handles) == 24);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, flags) == 28);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, first_signaled) == 32);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, pad) == 36);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineWait, deadline_nsec) == 40);
const _: () = assert!(size_of::<DrmSyncobjEventfd>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjEventfd, handle) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjEventfd, flags) == 4);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjEventfd, point) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjEventfd, fd) == 16);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjEventfd, pad) == 20);
const _: () = assert!(size_of::<DrmSyncobjArray>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjArray, handles) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjArray, count_handles) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjArray, pad) == 12);
const _: () = assert!(size_of::<DrmSyncobjTimelineArray>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineArray, handles) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineArray, points) == 8);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineArray, count_handles) == 16);
const _: () = assert!(core::mem::offset_of!(DrmSyncobjTimelineArray, flags) == 20);
const _: () = assert!(size_of::<DrmCrtcGetSequence>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmCrtcGetSequence, crtc_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcGetSequence, active) == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtcGetSequence, sequence) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcGetSequence, sequence_ns) == 16);
const _: () = assert!(size_of::<DrmCrtcQueueSequence>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmCrtcQueueSequence, crtc_id) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcQueueSequence, flags) == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtcQueueSequence, sequence) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcQueueSequence, user_data) == 16);
const _: () = assert!(size_of::<DrmSetClientName>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmSetClientName, name_len) == 0);
const _: () = assert!(core::mem::offset_of!(DrmSetClientName, name) == 8);
const _: () = assert!(size_of::<DrmEvent>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmEvent, r#type) == 0);
const _: () = assert!(core::mem::offset_of!(DrmEvent, length) == 4);
const _: () = assert!(size_of::<DrmEventVblank>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmEventVblank, base) == 0);
const _: () = assert!(size_of::<DrmEvent>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmEventVblank, user_data) == 8);
const _: () = assert!(core::mem::offset_of!(DrmEventVblank, tv_sec) == 16);
const _: () = assert!(core::mem::offset_of!(DrmEventVblank, tv_usec) == 20);
const _: () = assert!(core::mem::offset_of!(DrmEventVblank, sequence) == 24);
const _: () = assert!(core::mem::offset_of!(DrmEventVblank, crtc_id) == 28);
const _: () = assert!(size_of::<DrmEventCrtcSequence>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmEventCrtcSequence, base) == 0);
const _: () = assert!(size_of::<DrmEvent>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmEventCrtcSequence, user_data) == 8);
const _: () = assert!(core::mem::offset_of!(DrmEventCrtcSequence, time_ns) == 16);
const _: () = assert!(core::mem::offset_of!(DrmEventCrtcSequence, sequence) == 24);
