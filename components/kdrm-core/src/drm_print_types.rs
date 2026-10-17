// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Field-for-field mirror of the Linux v7.2.3 x86_64 definitions below,
// generated from the configured reference build's DWARF layout (pahole). Every
// size and field offset is pinned by a compile-time assertion.
//! Linux DRM type definitions. Source headers:
//! `include/drm/drm_print.h`
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use core::{
    ffi::{c_char, c_void},
    mem::size_of,
};

#[repr(C)]
pub struct DrmPrinterLine {
    pub series: u32,
    pub counter: u32,
}

#[repr(C)]
pub struct DrmPrinter {
    pub printfn: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut c_void)>,
    pub puts: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut c_char)>,
    pub arg: *mut c_void,
    pub origin: *mut c_void,
    pub prefix: *mut c_char,
    pub line: DrmPrinterLine,
    pub category: DrmDebugCategory,
}

#[repr(C)]
pub struct DrmPrintIterator {
    pub data: *mut c_void,
    pub start: isize,
    pub remain: isize,
    pub offset: isize,
}

pub type DrmDebugCategory = u32;
pub const DRM_UT_CORE: DrmDebugCategory = 0;
pub const DRM_UT_DRIVER: DrmDebugCategory = 1;
pub const DRM_UT_KMS: DrmDebugCategory = 2;
pub const DRM_UT_PRIME: DrmDebugCategory = 3;
pub const DRM_UT_ATOMIC: DrmDebugCategory = 4;
pub const DRM_UT_VBL: DrmDebugCategory = 5;
pub const DRM_UT_STATE: DrmDebugCategory = 6;
pub const DRM_UT_LEASE: DrmDebugCategory = 7;
pub const DRM_UT_DP: DrmDebugCategory = 8;
pub const DRM_UT_DRMRES: DrmDebugCategory = 9;

const _: () = assert!(size_of::<DrmPrinterLine>() == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrinterLine, series) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrinterLine, counter) == 4);
const _: () = assert!(size_of::<DrmPrinter>() == 56);
const _: () = assert!(core::mem::offset_of!(DrmPrinter, printfn) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrinter, puts) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrinter, arg) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPrinter, origin) == 24);
const _: () = assert!(core::mem::offset_of!(DrmPrinter, prefix) == 32);
const _: () = assert!(core::mem::offset_of!(DrmPrinter, line) == 40);
const _: () = assert!(core::mem::offset_of!(DrmPrinter, category) == 48);
const _: () = assert!(size_of::<DrmDebugCategory>() == 4);
const _: () = assert!(size_of::<DrmPrintIterator>() == 32);
const _: () = assert!(core::mem::offset_of!(DrmPrintIterator, data) == 0);
const _: () = assert!(core::mem::offset_of!(DrmPrintIterator, start) == 8);
const _: () = assert!(core::mem::offset_of!(DrmPrintIterator, remain) == 16);
const _: () = assert!(core::mem::offset_of!(DrmPrintIterator, offset) == 24);
