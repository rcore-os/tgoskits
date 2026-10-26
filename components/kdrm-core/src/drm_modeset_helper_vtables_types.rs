// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_modeset_helper_vtables.h`

use core::{ffi::c_void, mem::size_of};

use crate::{
    drm_atomic_types::DrmAtomicCommit,
    drm_connector_types::{DrmConnector, DrmConnectorState, DrmConnectorStatus},
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_encoder_types::DrmEncoder,
    drm_framebuffer_types::DrmFramebuffer,
    drm_modes_types::{DrmDisplayMode, DrmModeStatus},
    drm_modeset_lock_types::DrmModesetAcquireCtx,
    drm_plane_types::{DrmPlane, DrmPlaneState},
};

#[repr(C)]
pub struct DrmCrtcHelperFuncs {
    pub dpms: Option<unsafe extern "C" fn(*mut DrmCrtc, i32)>,
    pub prepare: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub commit: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub mode_valid:
        Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmDisplayMode) -> DrmModeStatus>,
    pub mode_fixup: Option<
        unsafe extern "C" fn(*mut DrmCrtc, *mut DrmDisplayMode, *mut DrmDisplayMode) -> bool,
    >,
    pub mode_set: Option<
        unsafe extern "C" fn(
            *mut DrmCrtc,
            *mut DrmDisplayMode,
            *mut DrmDisplayMode,
            i32,
            i32,
            *mut DrmFramebuffer,
        ) -> i32,
    >,
    pub mode_set_nofb: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub mode_set_base:
        Option<unsafe extern "C" fn(*mut DrmCrtc, i32, i32, *mut DrmFramebuffer) -> i32>,
    pub disable: Option<unsafe extern "C" fn(*mut DrmCrtc)>,
    pub atomic_check: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmAtomicCommit) -> i32>,
    pub atomic_begin: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmAtomicCommit)>,
    pub atomic_flush: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmAtomicCommit)>,
    pub atomic_enable: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmAtomicCommit)>,
    pub atomic_disable: Option<unsafe extern "C" fn(*mut DrmCrtc, *mut DrmAtomicCommit)>,
    pub get_scanout_position: Option<
        unsafe extern "C" fn(
            *mut DrmCrtc,
            bool,
            *mut i32,
            *mut i32,
            *mut i64,
            *mut i64,
            *mut DrmDisplayMode,
        ) -> bool,
    >,
    pub handle_vblank_timeout: Option<unsafe extern "C" fn(*mut DrmCrtc) -> bool>,
}

#[repr(C)]
pub struct DrmEncoderHelperFuncs {
    pub dpms: Option<unsafe extern "C" fn(*mut DrmEncoder, i32)>,
    pub mode_valid:
        Option<unsafe extern "C" fn(*mut DrmEncoder, *mut DrmDisplayMode) -> DrmModeStatus>,
    pub mode_fixup: Option<
        unsafe extern "C" fn(*mut DrmEncoder, *mut DrmDisplayMode, *mut DrmDisplayMode) -> bool,
    >,
    pub prepare: Option<unsafe extern "C" fn(*mut DrmEncoder)>,
    pub commit: Option<unsafe extern "C" fn(*mut DrmEncoder)>,
    pub mode_set:
        Option<unsafe extern "C" fn(*mut DrmEncoder, *mut DrmDisplayMode, *mut DrmDisplayMode)>,
    pub atomic_mode_set:
        Option<unsafe extern "C" fn(*mut DrmEncoder, *mut DrmCrtcState, *mut DrmConnectorState)>,
    pub detect:
        Option<unsafe extern "C" fn(*mut DrmEncoder, *mut DrmConnector) -> DrmConnectorStatus>,
    pub atomic_disable: Option<unsafe extern "C" fn(*mut DrmEncoder, *mut DrmAtomicCommit)>,
    pub atomic_enable: Option<unsafe extern "C" fn(*mut DrmEncoder, *mut DrmAtomicCommit)>,
    pub disable: Option<unsafe extern "C" fn(*mut DrmEncoder)>,
    pub enable: Option<unsafe extern "C" fn(*mut DrmEncoder)>,
    pub atomic_check: Option<
        unsafe extern "C" fn(*mut DrmEncoder, *mut DrmCrtcState, *mut DrmConnectorState) -> i32,
    >,
}

#[repr(C)]
pub struct DrmConnectorHelperFuncs {
    pub get_modes: Option<unsafe extern "C" fn(*mut DrmConnector) -> i32>,
    pub detect_ctx:
        Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmModesetAcquireCtx, bool) -> i32>,
    pub mode_valid:
        Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmDisplayMode) -> DrmModeStatus>,
    pub mode_valid_ctx: Option<
        unsafe extern "C" fn(
            *mut DrmConnector,
            *mut DrmDisplayMode,
            *mut DrmModesetAcquireCtx,
            *mut DrmModeStatus,
        ) -> i32,
    >,
    pub best_encoder: Option<unsafe extern "C" fn(*mut DrmConnector) -> *mut DrmEncoder>,
    pub atomic_best_encoder:
        Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmAtomicCommit) -> *mut DrmEncoder>,
    pub atomic_check: Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmAtomicCommit) -> i32>,
    pub atomic_commit: Option<unsafe extern "C" fn(*mut DrmConnector, *mut DrmAtomicCommit)>,
    pub prepare_writeback_job: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32>,
    pub cleanup_writeback_job: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    pub enable_hpd: Option<unsafe extern "C" fn(*mut DrmConnector)>,
    pub disable_hpd: Option<unsafe extern "C" fn(*mut DrmConnector)>,
}

#[repr(C)]
pub struct DrmPlaneHelperFuncs {
    pub prepare_fb: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmPlaneState) -> i32>,
    pub cleanup_fb: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmPlaneState)>,
    pub begin_fb_access: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmPlaneState) -> i32>,
    pub end_fb_access: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmPlaneState)>,
    pub atomic_check: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmAtomicCommit) -> i32>,
    pub atomic_update: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmAtomicCommit)>,
    pub atomic_enable: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmAtomicCommit)>,
    pub atomic_disable: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmAtomicCommit)>,
    pub atomic_async_check:
        Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmAtomicCommit, bool) -> i32>,
    pub atomic_async_update: Option<unsafe extern "C" fn(*mut DrmPlane, *mut DrmAtomicCommit)>,
    pub get_scanout_buffer: Option<unsafe extern "C" fn(*mut DrmPlane, *mut c_void) -> i32>,
    pub panic_flush: Option<unsafe extern "C" fn(*mut DrmPlane)>,
}

#[repr(C)]
pub struct DrmModeConfigHelperFuncs {
    pub atomic_commit_tail: Option<unsafe extern "C" fn(*mut DrmAtomicCommit)>,
    /// TGOS deviation from Linux's `atomic_commit_setup` callback: this slot is
    /// run after plane preparation and input-fence completion, immediately
    /// before state swap for blocking commits. NONBLOCK commits skip this
    /// fallible hook and publish software state before the worker waits for
    /// input fences. Returning nonzero aborts without changing current state.
    pub atomic_commit_pre_swap: Option<unsafe extern "C" fn(*mut DrmAtomicCommit) -> i32>,
}

const _: () = assert!(size_of::<DrmCrtcHelperFuncs>() == 128);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, dpms) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, prepare) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, commit) == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, mode_valid) == 24);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, mode_fixup) == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, mode_set) == 40);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, mode_set_nofb) == 48);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, mode_set_base) == 56);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, disable) == 64);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, atomic_check) == 72);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, atomic_begin) == 80);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, atomic_flush) == 88);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, atomic_enable) == 96);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, atomic_disable) == 104);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, get_scanout_position) == 112);
const _: () = assert!(core::mem::offset_of!(DrmCrtcHelperFuncs, handle_vblank_timeout) == 120);
const _: () = assert!(size_of::<DrmEncoderHelperFuncs>() == 104);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, dpms) == 0);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, mode_valid) == 8);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, mode_fixup) == 16);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, prepare) == 24);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, commit) == 32);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, mode_set) == 40);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, atomic_mode_set) == 48);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, detect) == 56);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, atomic_disable) == 64);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, atomic_enable) == 72);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, disable) == 80);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, enable) == 88);
const _: () = assert!(core::mem::offset_of!(DrmEncoderHelperFuncs, atomic_check) == 96);
const _: () = assert!(size_of::<DrmConnectorHelperFuncs>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, get_modes) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, detect_ctx) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, mode_valid) == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, mode_valid_ctx) == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, best_encoder) == 32);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, atomic_best_encoder) == 40);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, atomic_check) == 48);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, atomic_commit) == 56);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, prepare_writeback_job) == 64);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, cleanup_writeback_job) == 72);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, enable_hpd) == 80);
const _: () = assert!(core::mem::offset_of!(DrmConnectorHelperFuncs, disable_hpd) == 88);
const _: () = assert!(size_of::<DrmPlaneHelperFuncs>() == 96);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, prepare_fb) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, cleanup_fb) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, begin_fb_access) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, end_fb_access) == 24);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, atomic_check) == 32);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, atomic_update) == 40);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, atomic_enable) == 48);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, atomic_disable) == 56);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, atomic_async_check) == 64);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, atomic_async_update) == 72);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, get_scanout_buffer) == 80);
const _: () = assert!(core::mem::offset_of!(DrmPlaneHelperFuncs, panic_flush) == 88);
const _: () = assert!(size_of::<DrmModeConfigHelperFuncs>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigHelperFuncs, atomic_commit_tail) == 0);
const _: () = assert!(core::mem::offset_of!(DrmModeConfigHelperFuncs, atomic_commit_pre_swap) == 8);
