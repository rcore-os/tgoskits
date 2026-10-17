// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Port of Linux `drivers/gpu/drm/drm_print.c`.
//!
//! Source-faithful transcription of Linux 7.2.3 for the printer objects and
//! the drm logging entry points. Deviation: stable Rust cannot consume a C
//! `va_list` through this crate, so every `va_format` consumer takes the
//! already-formatted text as `&str` (or a `*const c_char` for the C-string
//! variants). The `printfn` callbacks receive a `*mut &str` in place of
//! `struct va_format *`. Output is routed to the kernel log through
//! `drm_kapi::print::drm_log_at`.
//!
//! The `debugfs_regset32` printer (`drm_print_regset32`) is not ported: its
//! only caller is debugfs, which is feature-gated out for this target.

use core::ffi::{c_char, c_void};

use drm_kapi::{
    print::{DrmLogLevel, drm_debug_enabled, drm_log_at},
    seq_file::{SeqFile, seq_write},
};

use crate::{
    drm_device_types::DrmDevice,
    drm_print_types::{DrmDebugCategory, DrmPrintIterator, DrmPrinter},
};

// upstream: drm_print.c __drm_debug_enabled(): `__drm_debug & BIT(category)`.
#[inline]
fn drm_debug_category_enabled(category: DrmDebugCategory) -> bool {
    drm_debug_enabled(1u32 << category)
}

// upstream: drm_print.c DRM_NAME
const DRM_NAME: &str = "drm";

fn cstr<'a>(s: *const c_char) -> &'a str {
    if s.is_null() {
        return "";
    }
    // SAFETY: `s` was checked non-null above; caller must ensure it points to a valid null-terminated C string.
    unsafe { core::ffi::CStr::from_ptr(s) }
        .to_str()
        .unwrap_or("")
}

fn cstr_len(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    // SAFETY: `s` was checked non-null above; caller must ensure it points to a valid null-terminated C string.
    unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes().len()
}

// The formatted text carried by a `struct va_format *` in this port: a
// `*mut &str` (see the module note).
/// # Safety
///
/// If `vaf` is non-null, it must point to a valid `&str` reference that outlives `'a`.
unsafe fn va_text<'a>(vaf: *mut c_void) -> &'a str {
    if vaf.is_null() {
        return "";
    }
    // SAFETY: `vaf` was checked non-null and is guaranteed by caller to point to a valid `&str` reference.
    unsafe { *(vaf as *mut &str) }
}

// upstream: drm_print.c __drm_dev_vprintk()
fn drm_dev_vprintk(level: DrmLogLevel, origin: *const c_void, prefix: Option<&str>, text: &str) {
    let prefix_pad = if prefix.is_some() { " " } else { "" };
    let prefix = prefix.unwrap_or("");
    let message = if origin.is_null() {
        alloc::format!("[{}]{}{} {}", DRM_NAME, prefix_pad, prefix, text)
    } else {
        alloc::format!(
            "[{}:{:p}]{}{} {}",
            DRM_NAME,
            origin,
            prefix_pad,
            prefix,
            text
        )
    };
    drm_log_at(level, "drm", file!(), line!(), &message);
}

// upstream: drm_print.c __drm_puts_coredump()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to an initialized `DrmPrinter` whose `arg` points to a valid `DrmPrintIterator`.
/// - `str` must point to a valid, null-terminated C string or be null.
/// - If `(*p).arg.data` is non-null, it must be writable for at least `(*p).arg.remain` bytes.
pub unsafe extern "C" fn __drm_puts_coredump(p: *mut DrmPrinter, str: *const c_char) {
    // SAFETY: Caller guarantees `p` is valid and dereferenceable, and `(*p).arg` points to a valid `DrmPrintIterator`.
    let it = unsafe { &mut *((*p).arg as *mut DrmPrintIterator) };

    if it.remain == 0 {
        return;
    }

    let len = cstr_len(str) as isize;
    if it.offset < it.start {
        if it.offset + len <= it.start {
            it.offset += len;
            return;
        }
        let mut copy = len - (it.start - it.offset);
        if copy > it.remain {
            copy = it.remain;
        }
        if !it.data.is_null() {
            // SAFETY: `it.data` is non-null, points to a writable buffer of size >= `copy`, and `str` has at least `copy` bytes from offset.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    str.add((it.start - it.offset) as usize).cast::<u8>(),
                    it.data as *mut u8,
                    copy as usize,
                );
            }
        }
        it.offset = it.start + copy;
        it.remain -= copy;
    } else {
        let pos = it.offset - it.start;
        let len = len.min(it.remain);
        if !it.data.is_null() {
            // SAFETY: `it.data` is non-null, writable at offset `pos`, and has capacity for at least `len` bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    str.cast::<u8>(),
                    (it.data as *mut u8).offset(pos),
                    len as usize,
                );
            }
        }
        it.offset += len;
        it.remain -= len;
    }
}

// upstream: drm_print.c __drm_printfn_coredump()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to an initialized `DrmPrinter` whose `arg` points to a valid `DrmPrintIterator`.
/// - `vaf` must point to a valid `&str` reference.
pub unsafe extern "C" fn __drm_printfn_coredump(p: *mut DrmPrinter, vaf: *mut c_void) {
    // SAFETY: Caller guarantees `vaf` points to a valid `&str` reference.
    let text = unsafe { va_text(vaf) };
    // SAFETY: Caller guarantees `p` is valid and `(*p).arg` points to a valid `DrmPrintIterator`.
    let it = unsafe { &mut *((*p).arg as *mut DrmPrintIterator) };

    if it.remain == 0 {
        return;
    }

    let len = text.len() as isize;
    if it.offset + len <= it.start {
        it.offset += len;
        return;
    }

    if it.offset >= it.start && len < it.remain {
        let pos = it.offset - it.start;
        if !it.data.is_null() {
            // SAFETY: `it.data` is non-null, writable at offset `pos`, and has capacity for `len + 1` bytes.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    text.as_ptr(),
                    (it.data as *mut u8).offset(pos),
                    len as usize,
                );
                *(it.data as *mut u8).offset(pos + len) = 0;
            }
        }
        it.offset += len;
        it.remain -= len;
        return;
    }

    // Linux kmallocs a temporary NUL-terminated copy and routes it through
    // __drm_puts_coredump().
    let buf = drm_kapi::memory::kmalloc(text.len() + 1, drm_kapi::config::GFP_KERNEL) as *mut u8;
    if buf.is_null() {
        return;
    }
    // SAFETY: `buf` was allocated with capacity `text.len() + 1`, is valid and non-overlapping with `text.as_ptr()`,
    // is null-terminated, passed to `__drm_puts_coredump`, and freed immediately before return.
    unsafe {
        core::ptr::copy_nonoverlapping(text.as_ptr(), buf, text.len());
        *buf.add(text.len()) = 0;
        __drm_puts_coredump(p, buf as *const c_char);
        drm_kapi::memory::kfree(buf);
    }
}

// upstream: drm_print.c __drm_puts_seq_file()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to an initialized `DrmPrinter` whose `arg` points to a valid `SeqFile`.
/// - `str` must point to a valid null-terminated C string.
pub unsafe extern "C" fn __drm_puts_seq_file(p: *mut DrmPrinter, str: *const c_char) {
    // SAFETY: Caller guarantees `p` is valid and dereferenceable, and `(*p).arg` points to a valid `SeqFile`.
    let m = unsafe { (*p).arg as *mut SeqFile };
    // SAFETY: `str` is a valid C string per caller contract, readable for `cstr_len(str)` bytes.
    let bytes = unsafe { core::slice::from_raw_parts(str.cast::<u8>(), cstr_len(str)) };
    // SAFETY: `m` points to a valid `SeqFile` and `bytes` is a valid slice.
    unsafe { seq_write(m, bytes) };
}

// upstream: drm_print.c __drm_printfn_seq_file()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to an initialized `DrmPrinter` whose `arg` points to a valid `SeqFile`.
/// - `vaf` must point to a valid `&str` reference.
pub unsafe extern "C" fn __drm_printfn_seq_file(p: *mut DrmPrinter, vaf: *mut c_void) {
    // SAFETY: Caller guarantees `p` is valid and dereferenceable, and `(*p).arg` points to a valid `SeqFile`.
    let m = unsafe { (*p).arg as *mut SeqFile };
    // SAFETY: Caller ensures `vaf` points to a valid `&str` reference; `m` points to a valid `SeqFile`.
    unsafe { seq_write(m, va_text(vaf).as_bytes()) };
}

// upstream: drm_print.c __drm_printfn_info()
#[unsafe(no_mangle)]
/// # Safety
///
/// `vaf` must point to a valid `&str` reference.
pub unsafe extern "C" fn __drm_printfn_info(_p: *mut DrmPrinter, vaf: *mut c_void) {
    // SAFETY: Caller guarantees `vaf` points to a valid `&str` reference.
    let text = unsafe { va_text(vaf) };
    drm_log_at(
        DrmLogLevel::Info,
        "drm",
        file!(),
        line!(),
        &alloc::format!("[{}] {}", DRM_NAME, text),
    );
}

// upstream: drm_print.c __drm_printfn_dbg()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to an initialized `DrmPrinter`.
/// - `vaf` must point to a valid `&str` reference.
pub unsafe extern "C" fn __drm_printfn_dbg(p: *mut DrmPrinter, vaf: *mut c_void) {
    // SAFETY: Caller guarantees `p` is valid and dereferenceable.
    let _drm = unsafe { (*p).arg as *const DrmDevice };
    // SAFETY: `p` is valid and dereferenceable.
    let category = unsafe { (*p).category };
    if !drm_debug_category_enabled(category) {
        return;
    }
    // SAFETY: `p` is valid and dereferenceable.
    let prefix = unsafe { (*p).prefix };
    let prefix = if prefix.is_null() {
        None
    } else {
        Some(cstr(prefix))
    };
    // SAFETY: `p` is valid and dereferenceable, and `vaf` points to a valid `&str` reference.
    drm_dev_vprintk(DrmLogLevel::Debug, unsafe { (*p).origin }, prefix, unsafe {
        va_text(vaf)
    });
}

// upstream: drm_print.c __drm_printfn_err()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to an initialized `DrmPrinter`.
/// - `vaf` must point to a valid `&str` reference.
pub unsafe extern "C" fn __drm_printfn_err(p: *mut DrmPrinter, vaf: *mut c_void) {
    // SAFETY: Caller guarantees `vaf` points to a valid `&str` reference.
    let text = unsafe { va_text(vaf) };
    // SAFETY: Caller guarantees `p` is valid and dereferenceable.
    let prefix = unsafe { (*p).prefix };
    let msg = if prefix.is_null() {
        alloc::string::String::from(text)
    } else {
        alloc::format!("{} {}", cstr(prefix), text)
    };
    drm_log_at(DrmLogLevel::Error, "drm", file!(), line!(), &msg);
}

// upstream: drm_print.c __drm_printfn_line()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must point to an initialized `DrmPrinter` with initialized `line` tracking.
/// - `vaf` must point to a valid `&str` reference.
pub unsafe extern "C" fn __drm_printfn_line(p: *mut DrmPrinter, vaf: *mut c_void) {
    // SAFETY: Caller guarantees `vaf` points to a valid `&str` reference.
    let text = unsafe { va_text(vaf) };
    // SAFETY: Caller guarantees `p` is valid, mutable, and dereferenceable.
    let counter = unsafe {
        (*p).line.counter = (*p).line.counter.wrapping_add(1);
        (*p).line.counter
    };
    // SAFETY: `p` is valid and dereferenceable.
    let prefix = unsafe { (*p).prefix };
    let prefix_text = if prefix.is_null() { "" } else { cstr(prefix) };
    let pad = if prefix.is_null() { "" } else { " " };
    // SAFETY: `p` is valid and dereferenceable.
    let series = unsafe { (*p).line.series };
    let head = if series != 0 {
        alloc::format!("{}{}{}.{}: ", prefix_text, pad, series, counter)
    } else {
        alloc::format!("{}{}{}: ", prefix_text, pad, counter)
    };
    // SAFETY: `p` is valid to pass to `drm_printf`.
    unsafe { drm_printf(p, &alloc::format!("{head}{text}")) };
}

// upstream: drm_print.c drm_puts()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must be null or point to an initialized `DrmPrinter`.
/// - `str` must point to a valid null-terminated C string.
pub unsafe extern "C" fn drm_puts(p: *mut DrmPrinter, str: *const c_char) {
    // SAFETY: `p` is either null or a valid `DrmPrinter` pointer.
    if let Some(puts) = unsafe { (*p).puts } {
        // SAFETY: Caller guarantees `p` and `str` are valid pointers suitable for `puts`.
        unsafe { puts(p, str as *mut c_char) };
    } else {
        // SAFETY: `p` and `cstr(str)` are valid inputs for `drm_printf`.
        unsafe { drm_printf(p, cstr(str)) };
    }
}

// upstream: drm_print.c drm_printf() / drm_vprintf(). `text` is the
// already-formatted message; dispatch goes through the printer's printfn.
/// # Safety
///
/// `p` must be null or point to an initialized `DrmPrinter`.
pub unsafe fn drm_printf(p: *mut DrmPrinter, text: &str) {
    if p.is_null() {
        return;
    }
    // SAFETY: `p` was checked non-null and points to a valid `DrmPrinter`.
    if let Some(printfn) = unsafe { (*p).printfn } {
        let mut text_ref: &str = text;
        // SAFETY: `text_ref` is a valid stack-allocated `&str` passed as `*mut c_void` per printfn convention.
        unsafe { printfn(p, &mut text_ref as *mut &str as *mut c_void) };
    }
}

// upstream: drm_print.c drm_print_bits()
/// # Safety
///
/// - `p` must be null or point to an initialized `DrmPrinter`.
/// - Each non-null pointer in `bits` must point to a valid null-terminated C string.
pub unsafe fn drm_print_bits(p: *mut DrmPrinter, value: u64, bits: &[*const c_char], nbits: u32) {
    let mut first = true;
    let mut nbits = nbits;
    if drm_kapi::assert::warn_on(nbits > u64::BITS) {
        nbits = u64::BITS;
    }
    for i in 0..nbits {
        if value & (1u64 << i) == 0 {
            continue;
        }
        let bit = bits.get(i as usize).copied().unwrap_or(core::ptr::null());
        if drm_kapi::assert::warn_on(bit.is_null()) {
            continue;
        }
        // SAFETY: `bit` is non-null and points to a valid C string per caller contract; `p` is valid or handled by `drm_printf`.
        unsafe {
            drm_printf(
                p,
                &alloc::format!("{}{}", if first { "" } else { "," }, cstr(bit)),
            );
        }
        first = false;
    }
    if first {
        // SAFETY: `p` is valid or handled by `drm_printf`.
        unsafe { drm_printf(p, "(none)") };
    }
}

// upstream: drm_print.c drm_dev_printk(); `text` is the formatted message.
pub fn drm_dev_printk(_dev: *const c_void, level: DrmLogLevel, text: &str) {
    drm_dev_vprintk(level, core::ptr::null(), None, text);
}

// upstream: drm_print.c __drm_dev_dbg(); `text` is the formatted message.
pub fn __drm_dev_dbg(_dev: *const c_void, category: DrmDebugCategory, text: &str) {
    if !drm_debug_category_enabled(category) {
        return;
    }
    drm_dev_vprintk(DrmLogLevel::Debug, core::ptr::null(), None, text);
}

// upstream: drm_print.c __drm_err(); `text` is the formatted message.
pub fn __drm_err(text: &str) {
    drm_dev_vprintk(DrmLogLevel::Error, core::ptr::null(), Some("*ERROR*"), text);
}

// upstream: drm_print.c drm_print_hex_dump()
#[unsafe(no_mangle)]
/// # Safety
///
/// - `p` must be null or point to an initialized `DrmPrinter`.
/// - `prefix` must be null or point to a valid null-terminated C string.
/// - `buf` must point to readable memory of at least `len` bytes.
pub unsafe extern "C" fn drm_print_hex_dump(
    p: *mut DrmPrinter,
    prefix: *const c_char,
    buf: *const u8,
    len: usize,
) {
    let prefix = cstr(prefix);
    let mut i = 0usize;
    while i < len {
        let bytes_per_line = (len - i).min(16);
        let mut line = alloc::string::String::from(prefix);
        for k in 0..bytes_per_line {
            // SAFETY: `i + k < len`, and caller guarantees `buf` is readable for `len` bytes.
            line.push_str(&alloc::format!("{:02x}", unsafe { *buf.add(i + k) }));
        }
        line.push('\n');
        // SAFETY: `p` is valid or handled by `drm_printf`.
        unsafe { drm_printf(p, &line) };
        i += 16;
    }
}
