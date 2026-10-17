// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `drivers/gpu/drm/drm_displayid_internal.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{ffi::c_void, mem::size_of};

#[repr(C)]
pub struct DisplayidHeader {
    pub rev: u8,
    pub bytes: u8,
    pub prod_id: u8,
    pub ext_count: u8,
}

#[repr(C)]
pub struct DisplayidBlock {
    pub tag: u8,
    pub rev: u8,
    pub num_bytes: u8,
}

#[repr(C)]
pub struct DisplayidTiledBlock {
    pub base: DisplayidBlock,
    pub tile_cap: u8,
    pub topo: [u8; 3],
    pub tile_size: [u8; 4],
    pub tile_pixel_bezel: [u8; 5],
    pub topology_id: [u8; 9],
}

#[repr(C)]
pub struct DisplayidDetailedTimings1 {
    pub pixel_clock: [u8; 3],
    pub flags: u8,
    pub hactive: u16,
    pub hblank: u16,
    pub hsync: u16,
    pub hsw: u16,
    pub vactive: u16,
    pub vblank: u16,
    pub vsync: u16,
    pub vsw: u16,
}

#[repr(C, packed)]
pub struct DisplayidDetailedTimingBlock {
    pub base: DisplayidBlock,
    pub timings: [DisplayidDetailedTimings1; 0],
}

#[repr(C, packed)]
pub struct DisplayidFormulaTimings9 {
    pub flags: u8,
    pub hactive: u16,
    pub vactive: u16,
    pub vrefresh: u8,
}

#[repr(C, packed)]
pub struct DisplayidFormulaTimingBlock {
    pub base: DisplayidBlock,
    pub timings: [DisplayidFormulaTimings9; 0],
}

#[repr(C)]
pub struct DisplayidVesaVendorSpecificBlock {
    pub base: DisplayidBlock,
    pub oui: [u8; 3],
    pub data_structure_type: u8,
    pub mso: u8,
}

#[repr(C)]
pub struct DisplayidIter {
    pub drm_edid: *mut c_void,
    pub section: *mut u8,
    pub length: i32,
    pub idx: i32,
    pub ext_index: i32,
    pub version: u8,
    pub primary_use: u8,
    pub quirks: u8,
}

const _: () = assert!(size_of::<DisplayidHeader>() == 4);
const _: () = assert!(core::mem::offset_of!(DisplayidHeader, rev) == 0);
const _: () = assert!(core::mem::offset_of!(DisplayidHeader, bytes) == 1);
const _: () = assert!(core::mem::offset_of!(DisplayidHeader, prod_id) == 2);
const _: () = assert!(core::mem::offset_of!(DisplayidHeader, ext_count) == 3);
const _: () = assert!(size_of::<DisplayidBlock>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidBlock, tag) == 0);
const _: () = assert!(core::mem::offset_of!(DisplayidBlock, rev) == 1);
const _: () = assert!(core::mem::offset_of!(DisplayidBlock, num_bytes) == 2);
const _: () = assert!(size_of::<DisplayidTiledBlock>() == 25);
const _: () = assert!(core::mem::offset_of!(DisplayidTiledBlock, base) == 0);
const _: () = assert!(size_of::<DisplayidBlock>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidTiledBlock, tile_cap) == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidTiledBlock, topo) == 4);
const _: () = assert!(size_of::<[u8; 3]>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidTiledBlock, tile_size) == 7);
const _: () = assert!(size_of::<[u8; 4]>() == 4);
const _: () = assert!(core::mem::offset_of!(DisplayidTiledBlock, tile_pixel_bezel) == 11);
const _: () = assert!(size_of::<[u8; 5]>() == 5);
const _: () = assert!(core::mem::offset_of!(DisplayidTiledBlock, topology_id) == 16);
const _: () = assert!(size_of::<[u8; 9]>() == 9);
const _: () = assert!(size_of::<DisplayidDetailedTimings1>() == 20);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, pixel_clock) == 0);
const _: () = assert!(size_of::<[u8; 3]>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, flags) == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, hactive) == 4);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, hblank) == 6);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, hsync) == 8);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, hsw) == 10);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, vactive) == 12);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, vblank) == 14);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, vsync) == 16);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimings1, vsw) == 18);
const _: () = assert!(size_of::<DisplayidDetailedTimingBlock>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimingBlock, base) == 0);
const _: () = assert!(size_of::<DisplayidBlock>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidDetailedTimingBlock, timings) == 3);
const _: () = assert!(size_of::<[DisplayidDetailedTimings1; 0]>() == 0);
const _: () = assert!(size_of::<DisplayidFormulaTimings9>() == 6);
const _: () = assert!(core::mem::offset_of!(DisplayidFormulaTimings9, flags) == 0);
const _: () = assert!(core::mem::offset_of!(DisplayidFormulaTimings9, hactive) == 1);
const _: () = assert!(core::mem::offset_of!(DisplayidFormulaTimings9, vactive) == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidFormulaTimings9, vrefresh) == 5);
const _: () = assert!(size_of::<DisplayidFormulaTimingBlock>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidFormulaTimingBlock, base) == 0);
const _: () = assert!(size_of::<DisplayidBlock>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidFormulaTimingBlock, timings) == 3);
const _: () = assert!(size_of::<[DisplayidFormulaTimings9; 0]>() == 0);
const _: () = assert!(size_of::<DisplayidVesaVendorSpecificBlock>() == 8);
const _: () = assert!(core::mem::offset_of!(DisplayidVesaVendorSpecificBlock, base) == 0);
const _: () = assert!(size_of::<DisplayidBlock>() == 3);
const _: () = assert!(core::mem::offset_of!(DisplayidVesaVendorSpecificBlock, oui) == 3);
const _: () = assert!(size_of::<[u8; 3]>() == 3);
const _: () =
    assert!(core::mem::offset_of!(DisplayidVesaVendorSpecificBlock, data_structure_type) == 6);
const _: () = assert!(core::mem::offset_of!(DisplayidVesaVendorSpecificBlock, mso) == 7);
const _: () = assert!(size_of::<DisplayidIter>() == 32);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, drm_edid) == 0);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, section) == 8);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, length) == 16);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, idx) == 20);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, ext_index) == 24);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, version) == 28);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, primary_use) == 29);
const _: () = assert!(core::mem::offset_of!(DisplayidIter, quirks) == 30);
