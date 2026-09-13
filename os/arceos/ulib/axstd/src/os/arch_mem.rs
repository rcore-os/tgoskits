//! Compiler-ABI memory operations for AArch64 kernel images.
//!
//! `aarch64-unknown-none-softfloat` enables `strict-align`, so every
//! struct-sized copy whose field offsets cannot prove 8-byte alignment
//! compiles to a call into `compiler_builtins`, whose portable `memcpy`
//! walks both ranges one byte at a time. Scheduler wake, park, and switch
//! paths move dozens of such records per context switch, which made that
//! byte loop a top instruction consumer in wakeup-latency profiles.
//!
//! This module provides the kernel image's `memcpy` instead. Wide accesses
//! are used whenever they stay naturally aligned: both ranges aligned copies
//! word-by-word, and an aligned destination with an unaligned source copies
//! through aligned loads combined by shifts. All other shapes keep the
//! portable byte loop. Naturally aligned accesses are permitted on every
//! memory type, including the MMU-off Device memory that early boot code
//! executes on, so no fast path can fault before page tables are enabled.

#[cfg(not(any(test, feature = "host-test")))]
use core::ffi::c_void;

/// # Safety
///
/// Callers must uphold the C/libc ABI contract: `src` must be readable and
/// `dst` writable for `n` bytes, and the ranges must not overlap.
#[cfg(not(any(test, feature = "host-test")))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: usize) -> *mut c_void {
    unsafe { dispatch_memcpy(dst.cast::<u8>(), src.cast::<u8>(), n) };
    dst
}

/// Same dispatch as the exported [`memcpy`], callable from tests on any host.
unsafe fn dispatch_memcpy(dst: *mut u8, src: *const u8, n: usize) {
    unsafe {
        let dst_aligned = (dst as usize) & 0x7 == 0;
        let src_aligned = (src as usize) & 0x7 == 0;
        if src_aligned {
            if dst_aligned {
                copy_aligned_words(dst.cast(), src.cast(), n);
            } else {
                copy_bytes(dst, src, n);
            }
        } else if dst_aligned && n >= 16 {
            copy_shifted_words(dst.cast(), src, n);
        } else {
            copy_bytes(dst, src, n);
        }
    }
}

/// # Safety
///
/// Both ranges must be 8-byte aligned and hold at least `n` accessible
/// bytes.
unsafe fn copy_aligned_words(mut dst: *mut u64, mut src: *const u64, mut n: usize) {
    unsafe {
        while n >= 8 {
            *dst = *src;
            dst = dst.add(1);
            src = src.add(1);
            n -= 8;
        }
        copy_bytes(dst.cast(), src.cast(), n);
    }
}

/// Copies an unaligned source range into an aligned destination.
///
/// Loads stay on 8-byte-aligned source addresses and each stored word is
/// assembled from the overlapping loaded pair by a funnel shift, so the
/// destination never receives a wide unaligned store. The trailing partial
/// word is copied first so the funnel can cover every full word.
///
/// Assembling the final word loads the aligned word that begins at or after
/// the logical end, which may read up to 7 bytes past `src + n`. That load
/// is kept only when it cannot cross a page boundary: the final in-range
/// byte proves its page is mapped, and any smaller page size supported by
/// the kernel (4 KiB or larger) has boundaries at 4 KiB multiples. When the
/// load would cross, the final word falls back to the byte loop.
///
/// # Safety
///
/// `dst` must be 8-byte aligned, `src` must not be, and both ranges must
/// hold at least `n` accessible bytes with `n >= 16`.
unsafe fn copy_shifted_words(dst: *mut u64, src: *const u8, n: usize) {
    /// The largest page size whose boundaries the final-load guard must
    /// respect; actual kernel pages are never smaller.
    const GUARD_PAGE_SIZE: usize = 4096;

    unsafe {
        let src_misalignment = (src as usize) & 0x7;
        let shift_bits = src_misalignment * 8;
        let words = n / 8;
        let tail_bytes = n - words * 8;
        copy_bytes(
            dst.cast::<u8>().add(n - tail_bytes),
            src.add(n - tail_bytes),
            tail_bytes,
        );
        let aligned = src.map_addr(|address| address & !0x7).cast::<u64>();
        let final_load = aligned.addr() + words * 8;
        let final_word_page_safe = final_load % GUARD_PAGE_SIZE <= GUARD_PAGE_SIZE - 8;
        let assembled_words = words - usize::from(!final_word_page_safe);
        let mut previous = *aligned;
        for i in 0..assembled_words {
            let next = *aligned.add(i + 1);
            *dst.add(i) = rotate_word_pair(previous, next, shift_bits);
            previous = next;
        }
        if !final_word_page_safe {
            let final_word = assembled_words * 8;
            copy_bytes(
                dst.cast::<u8>().add(final_word),
                src.add(final_word),
                words * 8 - final_word,
            );
        }
    }
}

/// Returns the `shift_bits/8`-byte-shifted window of the `[low, high]` pair.
const fn rotate_word_pair(low: u64, high: u64, shift_bits: usize) -> u64 {
    if shift_bits == 0 {
        low
    } else {
        (low >> shift_bits) | (high << (64 - shift_bits))
    }
}

/// # Safety
///
/// Both ranges must hold at least `n` accessible bytes.
unsafe fn copy_bytes(mut dst: *mut u8, mut src: *const u8, mut n: usize) {
    unsafe {
        while n > 0 {
            *dst = *src;
            dst = dst.add(1);
            src = src.add(1);
            n -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::dispatch_memcpy;

    fn run_copy(arena: &mut [u8], src_base: usize, dst_base: usize, n: usize) {
        let src_pattern = |i: usize| (src_base.wrapping_mul(31).wrapping_add(i * 7 + 3)) as u8;
        for i in 0..n {
            arena[src_base + i] = src_pattern(i);
        }
        for i in 0..n {
            arena[dst_base + i] = 0xA5;
        }
        unsafe {
            dispatch_memcpy(
                arena.as_mut_ptr().add(dst_base),
                arena.as_ptr().add(src_base),
                n,
            );
        }
        for i in 0..n {
            assert_eq!(
                arena[dst_base + i],
                src_pattern(i),
                "copy mismatch src={src_base} dst={dst_base} n={n} at {i}"
            );
        }
    }

    #[test]
    fn every_alignment_and_size_combination_copies_exactly() {
        let mut arena = vec![0u8; 512];
        for src_off in 0..16usize {
            for dst_off in 0..16usize {
                for n in 0..96usize {
                    run_copy(&mut arena, 128 + src_off, 256 + dst_off, n);
                }
            }
        }
    }

    #[test]
    fn page_boundary_sources_copy_exactly() {
        let mut arena = vec![0u8; 131072];
        let dst_base = 98304;
        for edge in (4096..61440).step_by(4096) {
            for delta in [
                0usize, 1, 4, 7, 8, 9, 4080, 4084, 4087, 4088, 4089, 4090, 4095, 4100, 4200,
            ] {
                for misalignment in 1..8usize {
                    for n in [24usize, 64, 72] {
                        run_copy(&mut arena, edge + delta + misalignment, dst_base, n);
                    }
                }
            }
        }
    }
}
