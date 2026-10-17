// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_atomic.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::c_void,
    mem::{align_of, size_of},
};

use drm_kapi::{
    completion::Completion,
    types::{Kref, ListHead, WorkStruct},
};

use crate::{
    drm_connector_types::{DrmConnector, DrmConnectorState},
    drm_crtc_types::{DrmCrtc, DrmCrtcState},
    drm_device_types::DrmDevice,
    drm_modeset_lock_types::{DrmModesetAcquireCtx, DrmModesetLock},
    drm_plane_types::{DrmPlane, DrmPlaneState},
    drm_print_types::DrmPrinter,
    drm_vblank_types::DrmPendingVblankEvent,
};

#[repr(C, align(8))]
pub struct DrmCrtcCommit {
    pub crtc: *mut DrmCrtc,
    pub r#ref: Kref,
    pub flip_done: Completion,
    pub hw_done: Completion,
    pub cleanup_done: Completion,
    pub commit_entry: ListHead,
    pub event: *mut DrmPendingVblankEvent,
    pub abort_completion: bool,
}

#[repr(C)]
pub struct DrmColoropsState {
    pub ptr: *mut c_void,
    pub state: *mut c_void,
    pub old_state: *mut c_void,
    pub new_state: *mut c_void,
}

#[repr(C)]
pub struct DrmPlanesState {
    pub ptr: *mut DrmPlane,
    pub state_to_destroy: *mut DrmPlaneState,
    pub old_state: *mut DrmPlaneState,
    pub new_state: *mut DrmPlaneState,
}

#[repr(C)]
pub struct DrmCrtcsState {
    pub ptr: *mut DrmCrtc,
    pub state_to_destroy: *mut DrmCrtcState,
    pub old_state: *mut DrmCrtcState,
    pub new_state: *mut DrmCrtcState,
    pub commit: *mut DrmCrtcCommit,
    pub out_fence_ptr: *mut i32,
    pub last_vblank_count: u64,
}

#[repr(C)]
pub struct DrmConnnectorsState {
    pub ptr: *mut DrmConnector,
    pub state_to_destroy: *mut DrmConnectorState,
    pub old_state: *mut DrmConnectorState,
    pub new_state: *mut DrmConnectorState,
    pub out_fence_ptr: *mut i32,
}

#[repr(C)]
pub struct DrmPrivateStateFuncs {
    pub atomic_create_state:
        Option<unsafe extern "C" fn(*mut DrmPrivateObj) -> *mut DrmPrivateState>,
    pub atomic_duplicate_state:
        Option<unsafe extern "C" fn(*mut DrmPrivateObj) -> *mut DrmPrivateState>,
    pub atomic_destroy_state:
        Option<unsafe extern "C" fn(*mut DrmPrivateObj, *mut DrmPrivateState)>,
    pub atomic_print_state: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut DrmPrivateState)>,
}

#[repr(C, align(8))]
pub struct DrmPrivateObj {
    pub dev: *mut DrmDevice,
    pub head: ListHead,
    pub lock: DrmModesetLock,
    pub state: *mut DrmPrivateState,
    pub funcs: *mut DrmPrivateStateFuncs,
}

#[repr(C)]
pub struct DrmPrivateState {
    pub state: *mut DrmAtomicCommit,
    pub obj: *mut DrmPrivateObj,
}

#[repr(C)]
pub struct DrmPrivateObjsState {
    pub ptr: *mut DrmPrivateObj,
    pub state_to_destroy: *mut DrmPrivateState,
    pub old_state: *mut DrmPrivateState,
    pub new_state: *mut DrmPrivateState,
}

#[repr(C, align(8))]
pub struct DrmAtomicCommit {
    pub r#ref: Kref,
    pub dev: *mut DrmDevice,
    /// bit-fields allow_modeset:1, legacy_cursor_update:1, async_update:1, duplicated:1, checked:1, plane_color_pipeline:1
    pub _bitfields_16: [u8; 1],
    pub colorops: *mut DrmColoropsState,
    pub planes: *mut DrmPlanesState,
    pub crtcs: *mut DrmCrtcsState,
    pub num_connector: i32,
    pub connectors: *mut DrmConnnectorsState,
    pub num_private_objs: i32,
    pub private_objs: *mut DrmPrivateObjsState,
    pub acquire_ctx: *mut DrmModesetAcquireCtx,
    pub fake_commit: *mut DrmCrtcCommit,
    pub commit_work: WorkStruct,
}

impl DrmAtomicCommit {
    #[inline]
    pub fn allow_modeset(&self) -> bool {
        (self._bitfields_16[0] & 1) != 0
    }

    #[inline]
    pub fn set_allow_modeset(&mut self, val: bool) {
        if val {
            self._bitfields_16[0] |= 1;
        } else {
            self._bitfields_16[0] &= !1;
        }
    }

    #[inline]
    pub fn legacy_cursor_update(&self) -> bool {
        (self._bitfields_16[0] & (1 << 1)) != 0
    }

    #[inline]
    pub fn set_legacy_cursor_update(&mut self, val: bool) {
        if val {
            self._bitfields_16[0] |= 1 << 1;
        } else {
            self._bitfields_16[0] &= !(1 << 1);
        }
    }

    #[inline]
    pub fn async_update(&self) -> bool {
        (self._bitfields_16[0] & (1 << 2)) != 0
    }

    #[inline]
    pub fn set_async_update(&mut self, val: bool) {
        if val {
            self._bitfields_16[0] |= 1 << 2;
        } else {
            self._bitfields_16[0] &= !(1 << 2);
        }
    }

    #[inline]
    pub fn duplicated(&self) -> bool {
        (self._bitfields_16[0] & (1 << 3)) != 0
    }

    #[inline]
    pub fn set_duplicated(&mut self, val: bool) {
        if val {
            self._bitfields_16[0] |= 1 << 3;
        } else {
            self._bitfields_16[0] &= !(1 << 3);
        }
    }

    #[inline]
    pub fn checked(&self) -> bool {
        (self._bitfields_16[0] & (1 << 4)) != 0
    }

    #[inline]
    pub fn set_checked(&mut self, val: bool) {
        if val {
            self._bitfields_16[0] |= 1 << 4;
        } else {
            self._bitfields_16[0] &= !(1 << 4);
        }
    }

    #[inline]
    pub fn plane_color_pipeline(&self) -> bool {
        (self._bitfields_16[0] & (1 << 5)) != 0
    }

    #[inline]
    pub fn set_plane_color_pipeline(&mut self, val: bool) {
        if val {
            self._bitfields_16[0] |= 1 << 5;
        } else {
            self._bitfields_16[0] &= !(1 << 5);
        }
    }
}

#[repr(C)]
pub struct DrmBusCfg {
    pub format: u32,
    pub flags: u32,
}

#[repr(C)]
pub struct DrmBridgeState {
    pub base: DrmPrivateState,
    pub bridge: *mut c_void,
    pub input_bus_cfg: DrmBusCfg,
    pub output_bus_cfg: DrmBusCfg,
}

const _: () = assert!(size_of::<DrmCrtcCommit>() == 144);
const _: () = assert!(align_of::<DrmCrtcCommit>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, crtc) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, r#ref) == 8);
const _: () = assert!(size_of::<Kref>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, flip_done) == 16);
const _: () = assert!(size_of::<Completion>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, hw_done) == 48);
const _: () = assert!(size_of::<Completion>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, cleanup_done) == 80);
const _: () = assert!(size_of::<Completion>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, commit_entry) == 112);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, event) == 128);
const _: () = assert!(core::mem::offset_of!(DrmCrtcCommit, abort_completion) == 136);
const _: () = assert!(size_of::<DrmColoropsState>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmColoropsState, ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmColoropsState, state) == 8);
const _: () = assert!(core::mem::offset_of!(DrmColoropsState, old_state) == 16);
const _: () = assert!(core::mem::offset_of!(DrmColoropsState, new_state) == 24);
const _: () = assert!(size_of::<DrmPlanesState>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmPlanesState, ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPlanesState, state_to_destroy) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPlanesState, old_state) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPlanesState, new_state) == 24);
const _: () = assert!(size_of::<DrmCrtcsState>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmCrtcsState, ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmCrtcsState, state_to_destroy) == 8);
const _: () = assert!(core::mem::offset_of!(DrmCrtcsState, old_state) == 16);
const _: () = assert!(core::mem::offset_of!(DrmCrtcsState, new_state) == 24);
const _: () = assert!(core::mem::offset_of!(DrmCrtcsState, commit) == 32);
const _: () = assert!(core::mem::offset_of!(DrmCrtcsState, out_fence_ptr) == 40);
const _: () = assert!(core::mem::offset_of!(DrmCrtcsState, last_vblank_count) == 48);
const _: () = assert!(size_of::<DrmConnnectorsState>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmConnnectorsState, ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmConnnectorsState, state_to_destroy) == 8);
const _: () = assert!(core::mem::offset_of!(DrmConnnectorsState, old_state) == 16);
const _: () = assert!(core::mem::offset_of!(DrmConnnectorsState, new_state) == 24);
const _: () = assert!(core::mem::offset_of!(DrmConnnectorsState, out_fence_ptr) == 32);
const _: () = assert!(size_of::<DrmPrivateStateFuncs>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmPrivateStateFuncs, atomic_create_state) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrivateStateFuncs, atomic_duplicate_state) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrivateStateFuncs, atomic_destroy_state) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPrivateStateFuncs, atomic_print_state) == 24);
const _: () = assert!(size_of::<DrmPrivateObj>() == 88);
const _: () = assert!(align_of::<DrmPrivateObj>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObj, dev) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObj, head) == 8);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObj, lock) == 24);
const _: () = assert!(size_of::<DrmModesetLock>() == 48);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObj, state) == 72);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObj, funcs) == 80);
const _: () = assert!(size_of::<DrmPrivateState>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmPrivateState, state) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrivateState, obj) == 8);
const _: () = assert!(size_of::<DrmPrivateObjsState>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObjsState, ptr) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObjsState, state_to_destroy) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObjsState, old_state) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPrivateObjsState, new_state) == 24);
const _: () = assert!(size_of::<DrmAtomicCommit>() == 128);
const _: () = assert!(align_of::<DrmAtomicCommit>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, r#ref) == 0);
const _: () = assert!(size_of::<Kref>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, dev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, _bitfields_16) == 16);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, colorops) == 24);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, planes) == 32);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, crtcs) == 40);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, num_connector) == 48);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, connectors) == 56);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, num_private_objs) == 64);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, private_objs) == 72);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, acquire_ctx) == 80);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, fake_commit) == 88);
const _: () = assert!(core::mem::offset_of!(DrmAtomicCommit, commit_work) == 96);
const _: () = assert!(size_of::<WorkStruct>() == 32);
const _: () = assert!(size_of::<DrmBusCfg>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmBusCfg, format) == 0);
const _: () = assert!(core::mem::offset_of!(DrmBusCfg, flags) == 4);
const _: () = assert!(size_of::<DrmBridgeState>() == 40);
const _: () = assert!(core::mem::offset_of!(DrmBridgeState, base) == 0);
const _: () = assert!(size_of::<DrmPrivateState>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmBridgeState, bridge) == 16);
const _: () = assert!(core::mem::offset_of!(DrmBridgeState, input_bus_cfg) == 24);
const _: () = assert!(size_of::<DrmBusCfg>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmBridgeState, output_bus_cfg) == 32);
const _: () = assert!(size_of::<DrmBusCfg>() == 8);
