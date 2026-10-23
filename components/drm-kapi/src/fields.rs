// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Generic Linux value types shared by the printk, time and lockdep shims:
// `ktime_t`, `va_list` formatting carriers, `struct drm_printer` and the
// lockdep class key stub. GPU-engine records stay with the driver translation.

use core::ffi::{c_char, c_void};

/// `ktime_t` from `include/linux/ktime.h` on the source x86_64 ABI.
pub type KtimeT = i64;

/// Rust spelling used for the same source `ktime_t` value by the engine file.
pub type Ktime = KtimeT;

/// `struct lock_class_key` with `CONFIG_LOCKDEP=n` in the task's Linux config.
/// Linux declares this as an empty C extension struct in that configuration;
/// its Rust representation is consequently zero-sized and is never embedded
/// in any runtime record here.
#[repr(C)]
pub struct LockClassKey {}

/// `va_format` from include/linux/kernel.h, used by DRM printer callbacks.
#[repr(C)]
pub struct VaFormat {
    pub fmt: *const c_char,
    pub va: *mut VaList,
}

/// Opaque C `va_list` storage; it is passed through to the configured printer
/// callback but never inspected or formatted by Rust.
#[repr(C)]
pub struct VaList {
    _opaque: [u8; 0],
}

/// `struct drm_printer` from include/drm/drm_print.h. Its callbacks and opaque
/// argument are the Linux ABI boundary; keep Rust-owned buffers out of this
/// record so source pointers retain the exact C layout.
#[repr(C)]
pub struct DrmPrinter {
    pub printfn: Option<unsafe extern "C" fn(*mut DrmPrinter, *mut VaFormat)>,
    pub puts: Option<unsafe extern "C" fn(*mut DrmPrinter, *const c_char)>,
    pub arg: *mut c_void,
    pub origin: *const c_void,
    pub prefix: *const c_char,
    pub line: DrmPrinterLine,
}

#[repr(C)]
pub struct DrmPrinterLine {
    pub series: u32,
    pub counter: u32,
}
