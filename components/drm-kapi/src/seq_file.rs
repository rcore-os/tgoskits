// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Small Linux `seq_file` output adapter for source-order DRM diagnostics.
//!
//! `seq_printf` is C-varargs in Linux; translated call sites pass a typed
//! argument slice to the same formatter used by DRM printf. The sequence-file
//! prefix is the Linux v7.2.3 ABI, and writes preserve its count/overflow and
//! NUL-reservation semantics so the enclosing read operation can retry with a
//! larger buffer.

use core::ffi::c_char;

use crate::print::CFormatArg;

/// Prefix of Linux `struct seq_file` through the state used by `seq_printf`.
/// The pointer passed to this type must be a live kernel-owned seq_file.
#[repr(C)]
pub struct SeqFile {
    pub buf: *mut c_char,
    pub size: usize,
    pub from: usize,
    pub count: usize,
    pub pad_until: usize,
}

/// Format and append one record using the kernel's bounded `seq_printf`
/// behavior. A failed/truncated append sets `count == size`, which is the
/// Linux seq-file overflow signal; the buffer owner must retry with more room.
///
/// # Safety
///
/// - `seq` must be non-null, properly aligned, and point to a valid, live `SeqFile` initialized for reads and writes.
/// - If `size > 0`, `(*seq).buf` must point to an allocated memory block of at least `size` bytes valid for reads and writes.
/// - `count` must not exceed `size`.
/// - The caller must ensure exclusive access to `*seq` and its underlying buffer for the duration of the call.
pub unsafe fn seq_printf(seq: *mut SeqFile, format: &str, args: &[&dyn CFormatArg]) {
    assert!(!seq.is_null());
    let message = crate::print::format_message(format, args);
    // SAFETY: `seq` is non-null per the assert above and is guaranteed valid, properly aligned, and initialized for reads per function safety contract.
    let count = unsafe { (*seq).count };
    // SAFETY: `seq` is non-null and valid for reads per function safety contract.
    let size = unsafe { (*seq).size };
    assert!(count <= size, "seq_file count exceeds its buffer size");

    if count == size {
        return;
    }
    // SAFETY: `seq` points to a valid, initialized `SeqFile` per function safety contract.
    let buffer = unsafe { (*seq).buf };
    assert!(
        !buffer.is_null(),
        "seq_file has available space but no buffer"
    );
    let available = size - count;
    if message.len() < available {
        // SAFETY:
        // - `buffer` is non-null per the preceding assert and valid for `size` bytes per caller contract.
        // - `available = size - count` and `message.len() < available`, so `count + message.len() < size`.
        // - `buffer.add(count)` and the NUL byte at `buffer.add(count + message.len())` are strictly within the `size` valid bytes of `buffer`.
        // - `message` and `buffer` do not overlap (message is newly formatted in local memory).
        // - `seq` is valid for writes per caller contract.
        unsafe {
            core::ptr::copy_nonoverlapping(
                message.as_ptr(),
                buffer.cast::<u8>().add(count),
                message.len(),
            );
            *buffer.cast::<u8>().add(count + message.len()) = 0;
            (*seq).count = count + message.len();
        }
    } else {
        // Linux vscnprintf/vsnprintf leaves a terminating NUL when there is
        // space and seq_set_overflow marks the whole buffer consumed.
        if available != 0 {
            let copied = available - 1;
            // SAFETY:
            // - `buffer` is non-null per the assert and valid for `size` bytes.
            // - `copied = available - 1 = size - count - 1`, so `count + copied = size - 1 < size`.
            // - `buffer.add(count)` and the NUL byte at `buffer.add(size - 1)` are within the `size` allocated bytes.
            // - `message` and `buffer` do not overlap.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    message.as_ptr(),
                    buffer.cast::<u8>().add(count),
                    copied,
                );
                *buffer.cast::<u8>().add(size - 1) = 0;
            }
        }
        // SAFETY: `seq` points to a live writable `SeqFile` per caller contract.
        unsafe { (*seq).count = size };
    }
}

/// Copy raw bytes using `seq_write()`'s all-or-overflow contract.
///
/// # Safety
///
/// - `seq` must be non-null, properly aligned, and point to a valid, live `SeqFile` initialized for reads and writes.
/// - If `size > 0`, `(*seq).buf` must point to an allocated memory block of at least `size` bytes valid for writes.
/// - `count` must not exceed `size`.
/// - The caller must ensure exclusive access to `*seq` and its underlying buffer for the duration of the call.
pub unsafe fn seq_write(seq: *mut SeqFile, bytes: &[u8]) {
    assert!(!seq.is_null());
    // SAFETY: `seq` is non-null per the assert above and valid for reads per function safety contract.
    let count = unsafe { (*seq).count };
    // SAFETY: `seq` is valid for reads per function safety contract.
    let size = unsafe { (*seq).size };
    assert!(count <= size, "seq_file count exceeds its buffer size");
    if bytes.len() > size - count {
        // SAFETY: `seq` points to a live writable `SeqFile` per caller contract.
        unsafe { (*seq).count = size };
        return;
    }
    // SAFETY: `seq` points to a valid `SeqFile` per function safety contract.
    assert!(!unsafe { (*seq).buf }.is_null() || bytes.is_empty());
    if !bytes.is_empty() {
        // SAFETY:
        // - `bytes.len() <= size - count`, so `count + bytes.len() <= size`.
        // - `(*seq).buf` is non-null per the assert above and valid for writes of `size` bytes per caller contract.
        // - `bytes` and destination buffer do not overlap.
        // - `seq` is valid for writes per caller contract.
        unsafe {
            core::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (*seq).buf.cast::<u8>().add(count),
                bytes.len(),
            );
            (*seq).count = count + bytes.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::*;

    #[test]
    fn formatted_write_uses_linux_count_and_overflow_semantics() {
        let mut buf = [0u8; 7];
        let mut seq = SeqFile {
            buf: buf.as_mut_ptr().cast(),
            size: buf.len(),
            from: 0,
            count: 0,
            pad_until: 0,
        };
        let number = 12u32;
        // SAFETY: `&mut seq` is a valid, aligned stack-allocated `SeqFile` whose `buf` points to a 7-byte stack buffer with `count = 0 <= size = 7`, and `seq` has exclusive access.
        unsafe { seq_printf(&mut seq, "v=%u", &[&number]) };
        assert_eq!(seq.count, 4);
        assert_eq!(&buf[..5], b"v=12\0");

        seq.count = 0;
        buf.fill(0);
        let long = "1234567";
        // SAFETY: `&mut seq` is a valid stack-allocated `SeqFile` with a 7-byte buffer, `count = 0 <= size = 7`, and exclusive access.
        unsafe { seq_printf(&mut seq, "%s", &[&long]) };
        assert_eq!(seq.count, seq.size);
        assert_eq!(buf[6], 0);

        seq.buf = ptr::null_mut();
        seq.size = 0;
        seq.count = 0;
        // SAFETY: `&mut seq` has `size = 0` and `count = 0`, satisfying the contract for empty writes with a null buffer.
        unsafe { seq_printf(&mut seq, "", &[]) };
        assert_eq!(seq.count, 0);
    }

    #[test]
    fn raw_write_overflows_as_a_unit() {
        let mut buf = [0u8; 3];
        let mut seq = SeqFile {
            buf: buf.as_mut_ptr().cast(),
            size: buf.len(),
            from: 0,
            count: 1,
            pad_until: 0,
        };
        // SAFETY: `&mut seq` is a valid stack-allocated `SeqFile` with a 3-byte buffer, `count = 1 <= size = 3`, and exclusive access.
        unsafe { seq_write(&mut seq, b"abc") };
        assert_eq!(seq.count, seq.size);
        assert_eq!(buf, [0; 3]);
    }
}

/// Linux `seq_puts()`: write the bytes of `text` up to its first NUL.
///
/// # Safety
///
/// `seq` must be non-null, properly aligned, and point to a live writable `SeqFile` with
/// buffer valid for writes up to `size`, with exclusive access, as required by [`seq_write`].
pub unsafe fn seq_puts(seq: *mut SeqFile, text: &str) {
    let bytes = text.as_bytes();
    let len = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    // SAFETY: `seq` satisfies all safety preconditions required by `seq_write` per the safety contract of `seq_puts`.
    unsafe { seq_write(seq, &bytes[..len]) };
}

/// Linux `seq_putc()`.
///
/// # Safety
///
/// `seq` must be non-null, properly aligned, and point to a live writable `SeqFile` with
/// buffer valid for writes up to `size`, with exclusive access, as required by [`seq_write`].
pub unsafe fn seq_putc(seq: *mut SeqFile, byte: u8) {
    // SAFETY: `seq` satisfies all safety preconditions required by `seq_write` per the safety contract of `seq_putc`, and `core::slice::from_ref` yields a valid 1-byte slice pointing to the local stack variable `byte`.
    unsafe { seq_write(seq, core::slice::from_ref(&byte)) };
}
