// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_device.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::{align_of, size_of},
    sync::atomic::AtomicBool,
};

use drm_kapi::{
    idr::Idr,
    types::{AtomicT, Kref, ListHead, Mutex, Spinlock},
};

use crate::{
    drm_auth_types::DrmMaster, drm_drv_types::DrmDriver, drm_fb_helper_types::DrmFbHelper,
    drm_file_types::DrmMinor, drm_mode_config_types::DrmModeConfig,
    drm_vblank_types::DrmVblankCrtc, drm_vma_manager_types::DrmVmaOffsetManager,
};

#[repr(C)]
pub struct DrmWedgeTaskInfo {
    pub pid: i32,
    pub comm: [c_char; 16],
}

#[repr(C, align(8))]
pub struct DrmDeviceManaged {
    pub resources: ListHead,
    pub final_kfree: *mut c_void,
    pub lock: Spinlock,
}

#[repr(C, align(8))]
pub struct DrmDevice {
    pub if_version: i32,
    pub r#ref: Kref,
    pub dev: *mut c_void,
    pub dma_dev: *mut c_void,
    pub managed: DrmDeviceManaged,
    pub driver: *mut DrmDriver,
    pub dev_private: *mut c_void,
    pub primary: *mut DrmMinor,
    pub render: *mut DrmMinor,
    pub accel: *mut DrmMinor,
    pub registered: bool,
    pub master: *mut DrmMaster,
    pub huge_mnt: *mut c_void,
    pub driver_features: u32,
    pub unplugged: AtomicBool,
    pub anon_inode: *mut c_void,
    pub unique: *mut c_char,
    pub master_mutex: Mutex,
    pub open_count: AtomicT,
    pub filelist_mutex: Mutex,
    pub filelist: ListHead,
    pub filelist_internal: ListHead,
    pub clientlist_mutex: Mutex,
    pub clientlist: ListHead,
    pub client_sysrq_list: ListHead,
    pub vblank_disable_immediate: bool,
    pub vblank: *mut DrmVblankCrtc,
    pub vblank_time_lock: Spinlock,
    pub vbl_lock: Spinlock,
    pub max_vblank_count: u32,
    pub vblank_event_list: ListHead,
    pub event_lock: Spinlock,
    pub num_crtcs: u32,
    pub mode_config: DrmModeConfig,
    pub object_name_lock: Mutex,
    pub object_name_idr: Idr,
    pub vma_offset_manager: *mut DrmVmaOffsetManager,
    pub vram_mm: *mut c_void,
    pub switch_power_state: SwitchPowerState,
    pub fb_helper: *mut DrmFbHelper,
    pub debugfs_root: *mut c_void,
    pub gem_lru_mutex: Mutex,
}

pub type SwitchPowerState = u32;
pub const DRM_SWITCH_POWER_ON: SwitchPowerState = 0;
pub const DRM_SWITCH_POWER_OFF: SwitchPowerState = 1;
pub const DRM_SWITCH_POWER_CHANGING: SwitchPowerState = 2;
pub const DRM_SWITCH_POWER_DYNAMIC_OFF: SwitchPowerState = 3;

const _: () = assert!(size_of::<DrmWedgeTaskInfo>() == 20);
const _: () = assert!(core::mem::offset_of!(DrmWedgeTaskInfo, pid) == 0);
const _: () = assert!(core::mem::offset_of!(DrmWedgeTaskInfo, comm) == 4);
const _: () = assert!(size_of::<[c_char; 16]>() == 16);
const _: () = assert!(size_of::<DrmDeviceManaged>() == 32);
const _: () = assert!(align_of::<DrmDeviceManaged>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmDeviceManaged, resources) == 0);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDeviceManaged, final_kfree) == 16);
const _: () = assert!(core::mem::offset_of!(DrmDeviceManaged, lock) == 24);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(size_of::<DrmDevice>() == 1560);
const _: () = assert!(align_of::<DrmDevice>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmDevice, if_version) == 0);
const _: () = assert!(core::mem::offset_of!(DrmDevice, r#ref) == 4);
const _: () = assert!(size_of::<Kref>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDevice, dev) == 8);
const _: () = assert!(core::mem::offset_of!(DrmDevice, dma_dev) == 16);
const _: () = assert!(core::mem::offset_of!(DrmDevice, managed) == 24);
const _: () = assert!(core::mem::offset_of!(DrmDevice, driver) == 56);
const _: () = assert!(core::mem::offset_of!(DrmDevice, dev_private) == 64);
const _: () = assert!(core::mem::offset_of!(DrmDevice, primary) == 72);
const _: () = assert!(core::mem::offset_of!(DrmDevice, render) == 80);
const _: () = assert!(core::mem::offset_of!(DrmDevice, accel) == 88);
const _: () = assert!(core::mem::offset_of!(DrmDevice, registered) == 96);
const _: () = assert!(core::mem::offset_of!(DrmDevice, master) == 104);
const _: () = assert!(core::mem::offset_of!(DrmDevice, huge_mnt) == 112);
const _: () = assert!(core::mem::offset_of!(DrmDevice, driver_features) == 120);
const _: () = assert!(core::mem::offset_of!(DrmDevice, unplugged) == 124);
const _: () = assert!(core::mem::offset_of!(DrmDevice, anon_inode) == 128);
const _: () = assert!(core::mem::offset_of!(DrmDevice, unique) == 136);
const _: () = assert!(core::mem::offset_of!(DrmDevice, master_mutex) == 144);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDevice, open_count) == 168);
const _: () = assert!(size_of::<AtomicT>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDevice, filelist_mutex) == 176);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDevice, filelist) == 200);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDevice, filelist_internal) == 216);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDevice, clientlist_mutex) == 232);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDevice, clientlist) == 256);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDevice, client_sysrq_list) == 272);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDevice, vblank_disable_immediate) == 288);
const _: () = assert!(core::mem::offset_of!(DrmDevice, vblank) == 296);
const _: () = assert!(core::mem::offset_of!(DrmDevice, vblank_time_lock) == 304);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDevice, vbl_lock) == 308);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDevice, max_vblank_count) == 312);
const _: () = assert!(core::mem::offset_of!(DrmDevice, vblank_event_list) == 320);
const _: () = assert!(size_of::<ListHead>() == 16);
const _: () = assert!(core::mem::offset_of!(DrmDevice, event_lock) == 336);
const _: () = assert!(size_of::<Spinlock>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDevice, num_crtcs) == 340);
const _: () = assert!(core::mem::offset_of!(DrmDevice, mode_config) == 344);
const _: () = assert!(size_of::<DrmModeConfig>() == 1104);
const _: () = assert!(core::mem::offset_of!(DrmDevice, object_name_lock) == 1448);
const _: () = assert!(size_of::<Mutex>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDevice, object_name_idr) == 1472);
const _: () = assert!(size_of::<Idr>() == 24);
const _: () = assert!(core::mem::offset_of!(DrmDevice, vma_offset_manager) == 1496);
const _: () = assert!(core::mem::offset_of!(DrmDevice, vram_mm) == 1504);
const _: () = assert!(core::mem::offset_of!(DrmDevice, switch_power_state) == 1512);
const _: () = assert!(size_of::<SwitchPowerState>() == 4);
const _: () = assert!(core::mem::offset_of!(DrmDevice, fb_helper) == 1520);
const _: () = assert!(core::mem::offset_of!(DrmDevice, debugfs_root) == 1528);
const _: () = assert!(core::mem::offset_of!(DrmDevice, gem_lru_mutex) == 1536);
const _: () = assert!(size_of::<Mutex>() == 24);
