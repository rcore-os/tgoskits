// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux bitmap allocation and aligned-region helpers used by GuC contexts.

use alloc::alloc::{Layout, alloc_zeroed, dealloc};
use core::{
    ffi::{c_int, c_long, c_ulong},
    ptr,
};

#[repr(C)]
struct BitmapAllocation {
    words: usize,
}

/// Linux `bitmap_zalloc()` with allocation metadata for the matching free.
/// The returned pointer addresses the first zeroed `unsigned long` word.
///
/// # Safety
///
/// - The returned bitmap must be freed only with [`bitmap_free`].
/// - The caller must not access bits at or beyond index `nbits`.
pub unsafe fn bitmap_zalloc(nbits: u32, _flags: u32) -> *mut c_ulong {
    let words = (nbits as usize).div_ceil(usize::BITS as usize);
    let size = core::mem::size_of::<BitmapAllocation>()
        .checked_add(words.saturating_mul(core::mem::size_of::<usize>()));
    let Some(size) = size else {
        return ptr::null_mut();
    };
    let Ok(layout) = Layout::from_size_align(size, core::mem::align_of::<usize>()) else {
        return ptr::null_mut();
    };
    // SAFETY: `layout` has non-zero size (at least `size_of::<BitmapAllocation>()`) and valid alignment (`align_of::<usize>()`) per `Layout::from_size_align`.
    let allocation = unsafe { alloc_zeroed(layout) };
    if allocation.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `allocation` is verified non-null above, properly aligned to `align_of::<usize>()` (which is >= `align_of::<BitmapAllocation>()`), and points to at least `size` bytes of allocated memory.
    unsafe {
        allocation
            .cast::<BitmapAllocation>()
            .write(BitmapAllocation { words })
    };
    // SAFETY: `allocation` points to `size` allocated bytes, where `size >= size_of::<BitmapAllocation>() + words * sizeof(usize)`. Adding `size_of::<BitmapAllocation>()` stays within the allocation and is aligned to `align_of::<usize>()`.
    unsafe {
        allocation
            .add(core::mem::size_of::<BitmapAllocation>())
            .cast()
    }
}

/// Linux `bitmap_free()` paired with `bitmap_zalloc()`.
///
/// # Safety
///
/// `bitmap` must be null or an unfreed pointer returned by [`bitmap_zalloc`].
pub unsafe fn bitmap_free(bitmap: *mut c_ulong) {
    if bitmap.is_null() {
        return;
    }
    // SAFETY: `bitmap` is non-null and was allocated by `bitmap_zalloc` with `size_of::<BitmapAllocation>()` prepended before the returned pointer. Subtracting this offset recovers the original allocation pointer.
    let allocation = unsafe {
        bitmap
            .cast::<u8>()
            .sub(core::mem::size_of::<BitmapAllocation>())
    };
    // SAFETY: `allocation` points to the `BitmapAllocation` header written during `bitmap_zalloc`, aligned to `align_of::<usize>()` and valid for reads.
    let words = unsafe { (*allocation.cast::<BitmapAllocation>()).words };
    let size = core::mem::size_of::<BitmapAllocation>() + words * core::mem::size_of::<usize>();
    let layout = Layout::from_size_align(size, core::mem::align_of::<usize>())
        .expect("bitmap allocation layout is recorded by bitmap_zalloc");
    // SAFETY: `allocation` was allocated with `layout` in `bitmap_zalloc`, matching the size and alignment recalculated from the stored `words` count, and is freed exactly once.
    unsafe { dealloc(allocation, layout) };
}

/// # Safety
///
/// `bitmap` must be non-null, aligned to `align_of::<c_ulong>()`, and valid for reads up to and including the word containing bit `bit`.
#[inline]
unsafe fn test_bit(bitmap: *const c_ulong, bit: usize) -> bool {
    // SAFETY: The caller guarantees `bitmap` is non-null, properly aligned, and valid for reads of the word at offset `bit / usize::BITS`.
    unsafe {
        *bitmap.add(bit / usize::BITS as usize) & ((1 as c_ulong) << (bit % c_ulong::BITS as usize))
            != 0
    }
}

/// # Safety
///
/// `bitmap` must be non-null, aligned to `align_of::<c_ulong>()`, and valid for reads and writes up to and including the word containing bit `bit`, with no concurrent unsynchronized access.
#[inline]
unsafe fn set_bit(bitmap: *mut c_ulong, bit: usize) {
    // SAFETY: The caller guarantees `bitmap` is non-null, properly aligned, valid for reads and writes at offset `bit / usize::BITS`, with exclusive access.
    unsafe {
        *bitmap.add(bit / usize::BITS as usize) |= (1 as c_ulong) << (bit % c_ulong::BITS as usize)
    };
}

/// # Safety
///
/// `bitmap` must be non-null, aligned to `align_of::<c_ulong>()`, and valid for reads and writes up to and including the word containing bit `bit`, with no concurrent unsynchronized access.
#[inline]
unsafe fn clear_bit(bitmap: *mut c_ulong, bit: usize) {
    // SAFETY: The caller guarantees `bitmap` is non-null, properly aligned, valid for reads and writes at offset `bit / usize::BITS`, with exclusive access.
    unsafe {
        *bitmap.add(bit / usize::BITS as usize) &=
            !((1 as c_ulong) << (bit % c_ulong::BITS as usize))
    };
}

/// Linux `bitmap_find_free_region()` for a power-of-two aligned region.
///
/// # Safety
///
/// `bitmap` must point to a valid, properly aligned, writable bitmap with storage for at least `nbits`
/// bits, initialized for exclusive access during the operation.
pub unsafe fn bitmap_find_free_region(bitmap: *mut c_ulong, nbits: u32, order: c_long) -> c_int {
    if bitmap.is_null() || order < 0 || order as u32 >= usize::BITS {
        return -1;
    }
    let Some(count) = 1usize.checked_shl(order as u32) else {
        return -1;
    };
    let nbits = nbits as usize;
    let mut start = 0usize;
    while start.checked_add(count).is_some_and(|end| end <= nbits) {
        // SAFETY: `start + bit < end <= nbits`, and `bitmap` is valid for at least `nbits` bits per caller preconditions.
        if (0..count).all(|bit| !unsafe { test_bit(bitmap, start + bit) }) {
            for bit in 0..count {
                // SAFETY: `start + bit < end <= nbits`, and `bitmap` is writable for `nbits` bits with exclusive access per caller preconditions.
                unsafe { set_bit(bitmap, start + bit) };
            }
            return start as c_int;
        }
        start += count;
    }
    -1
}

/// Linux `bitmap_release_region()` for a region reserved by the helper above.
///
/// # Safety
///
/// `bitmap` must point to a valid, properly aligned, writable bitmap with storage covering the region
/// beginning at `pos` of length `1 << order` bits, and no concurrent operation may modify those bits.
pub unsafe fn bitmap_release_region(bitmap: *mut c_ulong, pos: c_ulong, order: c_long) {
    if bitmap.is_null() || order < 0 || order as u32 >= usize::BITS {
        return;
    }
    let Some(count) = 1usize.checked_shl(order as u32) else {
        return;
    };
    for bit in 0..count {
        // SAFETY: `pos as usize + bit` is within the region guaranteed valid and writable by the caller, with exclusive access.
        unsafe { clear_bit(bitmap, pos as usize + bit) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_regions_are_reserved_and_released() {
        // SAFETY: Allocating a 32-bit bitmap with flags 0; will be freed below with `bitmap_free`.
        let bitmap = unsafe { bitmap_zalloc(32, 0) };
        assert!(!bitmap.is_null());
        // SAFETY: `bitmap` is a valid 32-bit bitmap allocated above, exclusively accessed, and freed at the end of the test.
        unsafe {
            assert_eq!(bitmap_find_free_region(bitmap, 32, 2), 0);
            assert_eq!(bitmap_find_free_region(bitmap, 32, 2), 4);
            bitmap_release_region(bitmap, 0, 2);
            assert_eq!(bitmap_find_free_region(bitmap, 32, 2), 0);
            bitmap_free(bitmap);
        }
    }
}
