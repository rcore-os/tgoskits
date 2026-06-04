//! `printk`/`snprintf`/`sprintf`/`memset` C-ABI shims used by every
//! loadable kernel module that calls into the Linux-side logging API.
//!
//! Ported from `Starry-OS/StarryOS:ebpf-kmod`
//! (`kernel/src/kmod/shim/kprint.rs`). Each exported function is
//! resolved by `KmodHelper::resolve_symbol` via the global kallsyms
//! table at module load time, so they must be referenced (not just
//! declared `pub`) in the kernel image — that's what `#[capi_fn]` from
//! `kmod-tools` arranges.

use core::{
    ffi::{VaList, c_char, c_int},
    ptr::null_mut,
};

use kmod::capi_fn;
use lwprintf_rs::SIZE_MAX;

/// ASCII SOH character. See
/// <https://elixir.bootlin.com/linux/v6.17/source/include/linux/kern_levels.h#L5>
#[allow(dead_code)]
const KERN_SOH: u8 = 0x01;

const KERN_EMERG: &[u8; 2] = &[0x01, b'0'];
const KERN_ALERT: &[u8; 2] = &[0x01, b'1'];
const KERN_CRIT: &[u8; 2] = &[0x01, b'2'];
const KERN_ERR: &[u8; 2] = &[0x01, b'3'];
const KERN_WARNING: &[u8; 2] = &[0x01, b'4'];
const KERN_NOTICE: &[u8; 2] = &[0x01, b'5'];
const KERN_INFO: &[u8; 2] = &[0x01, b'6'];
const KERN_DEBUG: &[u8; 2] = &[0x01, b'7'];

const LOG_LEVELS: &[&[u8; 2]] = &[
    KERN_EMERG,
    KERN_ALERT,
    KERN_CRIT,
    KERN_ERR,
    KERN_WARNING,
    KERN_NOTICE,
    KERN_INFO,
    KERN_DEBUG,
];

/// `memset` shim. C ABI; some modules call this directly rather than
/// through the compiler's intrinsic.
#[cfg(not(arceos_std))]
#[capi_fn]
pub unsafe extern "C" fn memset(s: *mut core::ffi::c_void, c: c_int, n: usize) -> *mut c_char {
    let xs = s as *mut u8;
    let byte = c as u8;
    for i in 0..n {
        // SAFETY: caller-guaranteed validity of `s` for `n` bytes (C contract).
        unsafe { *xs.add(i) = byte };
    }
    s as *mut c_char
}

/// Per-character writer that `lwprintf_vprintf_ex` calls back into.
#[capi_fn]
unsafe extern "C" fn write_char(c: u8) {
    ax_print!("{}", c as char);
}

/// Shared body for the `printk`-family entry points. Takes an already
/// `va_start`-ed `VaList` so that both `_printk` and `__warn_printk` can
/// forward their *own* variadic arguments here. Strips a leading `KERN_*`
/// level prefix and re-routes to `ax_print!`-backed `lwprintf`.
unsafe fn vprintk(fmt: *const c_char, args: VaList) -> i32 {
    // SAFETY: `fmt` is C-ABI; we treat it as a NUL-terminated string.
    let c_str_fmt = unsafe { core::ffi::CStr::from_ptr(fmt) };
    let fmt_bytes = c_str_fmt.to_bytes();
    let level_prefix = LOG_LEVELS
        .iter()
        .find(|&&level| fmt_bytes.starts_with(level))
        .copied();
    let trimmed = if let Some(level) = level_prefix {
        &fmt_bytes[level.len()..]
    } else {
        fmt_bytes
    };
    match level_prefix {
        Some(KERN_EMERG) | Some(KERN_ALERT) | Some(KERN_CRIT) | Some(KERN_ERR) => {
            ax_print!("[ERROR] ");
        }
        Some(KERN_WARNING) => {
            ax_print!("[WARN] ");
        }
        Some(KERN_NOTICE) | Some(KERN_INFO) => {
            ax_print!("[INFO] ");
        }
        Some(KERN_DEBUG) => {
            ax_print!("[DEBUG] ");
        }
        _ => {
            ax_print!("[INFO] ");
        }
    }
    // SAFETY: `trimmed` points into the same NUL-terminated string
    // (offset by the level prefix length, which is shorter than the
    // total). The caller's `VaList` is forwarded as-is.
    unsafe { lwprintf_rs::lwprintf_vprintf_ex(null_mut(), trimmed.as_ptr() as _, args) }
}

/// Linux `printk(fmt, ...)`.
#[capi_fn]
unsafe extern "C" fn _printk(fmt: *const c_char, args: ...) -> i32 {
    // SAFETY: forward this call's own variadic list to the shared body.
    unsafe { vprintk(fmt, args) }
}

/// `__warn_printk` alias used by `WARN_ON_ONCE` and friends.
#[capi_fn]
unsafe extern "C" fn __warn_printk(fmt: *const c_char, args: ...) -> i32 {
    // SAFETY: forward this call's own variadic list — *not* `_printk`'s — to
    // the shared body, so the arguments are not mis-shifted by one slot.
    unsafe { vprintk(fmt, args) }
}

/// `snprintf(buf, size, fmt, ...)`.
#[capi_fn]
unsafe extern "C" fn snprintf(
    buf: *mut c_char,
    size: usize,
    fmt: *const c_char,
    args: ...
) -> c_int {
    // SAFETY: caller-supplied buffer + format. We hand them through to
    // the C-side lwprintf which is itself memory-safe-by-contract.
    unsafe { lwprintf_rs::lwprintf_vsnprintf_ex(null_mut(), buf, size, fmt, args) }
}

/// `sprintf(buf, fmt, ...)`. Equivalent to `snprintf` with `SIZE_MAX`.
#[capi_fn]
unsafe extern "C" fn sprintf(buf: *mut c_char, fmt: *const c_char, args: ...) -> c_int {
    // SAFETY: same as `snprintf`.
    unsafe { lwprintf_rs::lwprintf_vsnprintf_ex(null_mut(), buf, SIZE_MAX as _, fmt, args) }
}
