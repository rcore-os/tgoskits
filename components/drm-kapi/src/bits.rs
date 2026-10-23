// SPDX-License-Identifier: MIT
// Copyright © 2026 Intel Corporation and TheKernel contributors.
// Linux bitops/bitfield operations used by the v7.2.3 DRM headers.

use core::{
    ffi::c_ulong,
    sync::atomic::{AtomicI32, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

pub trait BitWord: Copy {
    fn to_u64(self) -> u64;
    fn from_u64(value: u64) -> Self;
    fn load(ptr: *const Self) -> u64;
    fn fetch_or(ptr: *mut Self, mask: u64) -> u64;
    fn fetch_and(ptr: *mut Self, mask: u64) -> u64;
    fn store(ptr: *mut Self, value: u64);
}

#[macro_export]
macro_rules! impl_word {
    ($ty:ty, $atomic:ty, $bits:ty) => {
        impl BitWord for $ty {
            fn to_u64(self) -> u64 {
                self as u64
            }
            fn from_u64(value: u64) -> Self {
                value as Self
            }
            fn load(ptr: *const Self) -> u64 {
                // SAFETY: `ptr` points to a live, properly aligned `$ty` whose size and alignment match `$atomic`, safe for atomic load.
                unsafe { &*ptr.cast::<$atomic>() }.load(Ordering::Acquire) as u64
            }
            fn fetch_or(ptr: *mut Self, mask: u64) -> u64 {
                // SAFETY: `ptr` points to a live, properly aligned `$ty` whose size and alignment match `$atomic`, safe for atomic RMW.
                unsafe { &*ptr.cast::<$atomic>() }.fetch_or(mask as $bits, Ordering::AcqRel) as u64
            }
            fn fetch_and(ptr: *mut Self, mask: u64) -> u64 {
                // SAFETY: `ptr` points to a live, properly aligned `$ty` whose size and alignment match `$atomic`, safe for atomic RMW.
                unsafe { &*ptr.cast::<$atomic>() }.fetch_and(mask as $bits, Ordering::AcqRel) as u64
            }
            fn store(ptr: *mut Self, value: u64) {
                // SAFETY: `ptr` points to a live, properly aligned `$ty` whose size and alignment match `$atomic`, safe for atomic store.
                unsafe { &*ptr.cast::<$atomic>() }.store(value as $bits, Ordering::Release)
            }
        }
    };
}

impl_word!(u32, AtomicU32, u32);
impl_word!(i32, AtomicU32, u32);
impl_word!(u64, AtomicU64, u64);
impl_word!(i64, AtomicU64, u64);
impl_word!(usize, AtomicU64, u64);

pub trait BitIndex: Copy {
    fn index(self) -> u32;
}
impl BitIndex for u32 {
    fn index(self) -> u32 {
        self
    }
}
impl BitIndex for i32 {
    fn index(self) -> u32 {
        self as u32
    }
}
impl BitIndex for usize {
    fn index(self) -> u32 {
        self as u32
    }
}

#[inline]
pub fn test_bit<I: BitIndex, W: BitWord>(bit: I, word: &W) -> bool {
    W::load(word) & (1u64 << (bit.index() % (core::mem::size_of::<W>() as u32 * 8))) != 0
}

#[inline]
pub fn set_bit<I: BitIndex, W: BitWord>(bit: I, word: &mut W) {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    let _ = W::fetch_or(word, 1u64 << bit);
}

#[inline]
pub fn clear_bit<I: BitIndex, W: BitWord>(bit: I, word: &mut W) {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    let _ = W::fetch_and(word, !(1u64 << bit));
}

/// Release-ordered Linux `clear_bit_unlock()` primitive.
#[inline]
pub fn clear_bit_unlock<I: BitIndex, W: BitWord>(bit: I, word: &mut W) {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    let _ = W::fetch_and(word, !(1u64 << bit));
}

/// Linux `clear_and_wake_up_bit()` with a shared waiter queue. Waking waiters
/// on unrelated bits is permitted; every waiter rechecks its own bit predicate.
pub fn clear_and_wake_up_bit_inner<I: BitIndex, W: BitWord>(bit: I, word: &mut W) {
    clear_bit_unlock(bit, word);
    crate::wait::wake_all();
}

/// Linux `clear_and_wake_up_bit()` C ABI. The wait-queue key is shared across
/// bit addresses; collisions only wake waiters to recheck their own predicate.
///
/// # Safety
///
/// - `word` must be non-null, properly aligned to `align_of::<c_ulong>()`, and point to a valid, live `c_ulong`.
/// - `word` must remain valid for the duration of this call and any concurrent wait checks.
/// - The caller must uphold exclusivity required for mutable reference creation, or ensure all concurrent accesses are atomic.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clear_and_wake_up_bit(bit: i32, word: *mut c_ulong) {
    assert!(bit >= 0 && !word.is_null());
    // SAFETY: `word` is verified non-null and `bit >= 0` by the assert above; caller contract requires `word` to be aligned and valid for `c_ulong`. Note: creating `&mut *word` requires exclusive access under Rust aliasing rules, but concurrent `wait_on_bit` callers may violate this.
    clear_and_wake_up_bit_inner(bit as u32, unsafe { &mut *word });
}

/// Linux `wait_on_bit()` over the same predicate-checked LinuxKPI wait queue.
///
/// # Safety
///
/// - `word` must be non-null, properly aligned to `align_of::<c_ulong>()`, and point to a valid, live `c_ulong`.
/// - `word` must remain valid and readable for the entire duration of the wait loop.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wait_on_bit(word: *mut c_ulong, bit: i32, state: i32) -> i32 {
    if word.is_null() || bit < 0 {
        return -crate::config::EINVAL;
    }
    // SAFETY: `word` is checked non-null above, and points to a live, properly aligned `c_ulong` valid for concurrent atomic reads via `BitWord`.
    let word = unsafe { &*word };
    let condition = || !test_bit(bit as u32, word);
    if state as u32 & crate::wait::TASK_INTERRUPTIBLE != 0 {
        loop {
            if condition() {
                return 0;
            }
            if crate::signal::signal_pending_current() {
                return -crate::config::EINTR;
            }
            let ready = || condition() || crate::signal::signal_pending_current();
            let _ = crate::wait::wait_timeout_until(Some(10_000_000), &ready);
        }
    } else {
        let _ = crate::wait::wait_until_ready(None, &condition);
        0
    }
}

#[inline]
pub fn test_and_set_bit<I: BitIndex, W: BitWord>(bit: I, word: &mut W) -> bool {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    W::fetch_or(word, 1u64 << bit) & (1u64 << bit) != 0
}

#[inline]
pub fn test_and_clear_bit<I: BitIndex, W: BitWord>(bit: I, word: &mut W) -> bool {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    W::fetch_and(word, !(1u64 << bit)) & (1u64 << bit) != 0
}

#[inline]
pub fn __test_and_set_bit<I: BitIndex, W: BitWord>(bit: I, word: &mut W) -> bool {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    let old = W::load(word);
    W::store(word, old | (1u64 << bit));
    old & (1u64 << bit) != 0
}

#[inline]
pub fn __set_bit<I: BitIndex, W: BitWord>(bit: I, word: &mut W) {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    let old = W::load(word);
    W::store(word, old | (1u64 << bit));
}

#[inline]
pub fn __clear_bit<I: BitIndex, W: BitWord>(bit: I, word: &mut W) {
    let bit = bit.index() % (core::mem::size_of::<W>() as u32 * 8);
    let old = W::load(word);
    W::store(word, old & !(1u64 << bit));
}

#[inline]
pub fn lower_32_bits<W: BitWord>(value: W) -> u32 {
    value.to_u64() as u32
}

#[inline]
pub fn upper_32_bits<W: BitWord>(value: W) -> u32 {
    (value.to_u64() >> 32) as u32
}

#[inline]
pub fn field_prep(mask: u32, value: u32) -> u32 {
    if mask == 0 {
        0
    } else {
        (value << mask.trailing_zeros()) & mask
    }
}

#[inline]
pub const fn field_fit(mask: u32, value: u32) -> bool {
    value <= (mask >> mask.trailing_zeros())
}

#[inline]
pub const fn order_base_2(n: u32) -> u32 {
    if n <= 1 {
        0
    } else {
        u32::BITS - (n - 1).leading_zeros()
    }
}

#[inline]
pub const fn bit(n: u32) -> u32 {
    1u32.wrapping_shl(n)
}

/// Linux `CIRC_SPACE(head, tail, size)`: argument order is head first, then tail.
#[inline]
pub const fn circ_space(head: u32, tail: u32, size: u32) -> u32 {
    tail.wrapping_sub(head.wrapping_add(1)) & (size - 1)
}

#[inline]
pub fn for_each_set_bit(word: u64, limit: u32) -> impl Iterator<Item = u32> {
    (0..limit).filter(move |bit| word & (1u64 << bit) != 0)
}

#[inline]
pub fn is_aligned<V: BitWord, A: BitWord>(value: V, alignment: A) -> bool {
    let alignment = alignment.to_u64();
    alignment != 0 && value.to_u64() & (alignment - 1) == 0
}

/// Exact-value Linux `cmpxchg()` operation. x86 LOCK CMPXCHG provides full ordering.
///
/// # Safety
///
/// Implementors must ensure that:
/// - `Self` has the same size, alignment, and memory representation as an atomic type supporting compare-exchange.
/// - Implementations of `cmpxchg` perform atomic compare-and-swap operations with sequential consistency semantics.
pub unsafe trait CmpxchgValue: Copy {
    /// Compare and conditionally replace one atomic value.
    ///
    /// # Safety
    ///
    /// `pointer` must point to a live, properly aligned atomic location of
    /// `Self` that is not accessed non-atomically while this operation runs.
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self;
}

// SAFETY: `*mut T` has identical layout and alignment to `AtomicPtr<T>`.
unsafe impl<T> CmpxchgValue for *mut T {
    /// # Safety
    ///
    /// See [`CmpxchgValue::cmpxchg`].
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self {
        // SAFETY: `pointer` is guaranteed by caller to point to a live, properly aligned atomic memory location for `*mut T`.
        unsafe { AtomicPtr::from_ptr(pointer) }
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap_or_else(|actual| actual)
    }
}

// SAFETY: `*const T` has identical layout and alignment to `AtomicPtr<T>`, and casting to `*mut T` matches the underlying pointer representation.
unsafe impl<T> CmpxchgValue for *const T {
    /// # Safety
    ///
    /// See [`CmpxchgValue::cmpxchg`].
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self {
        // SAFETY: `pointer` is guaranteed by caller to point to a live, properly aligned atomic memory location.
        unsafe { AtomicPtr::from_ptr(pointer.cast::<*mut T>()) }
            .compare_exchange(
                old.cast_mut(),
                new.cast_mut(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .unwrap_or_else(|actual| actual)
            .cast_const()
    }
}

// SAFETY: `u32` has identical layout and alignment to `AtomicU32`.
unsafe impl CmpxchgValue for u32 {
    /// # Safety
    ///
    /// See [`CmpxchgValue::cmpxchg`].
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self {
        // SAFETY: `pointer` is guaranteed by caller to point to a live, properly aligned `u32` atomic location.
        unsafe { AtomicU32::from_ptr(pointer) }
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap_or_else(|actual| actual)
    }
}

// SAFETY: `u64` has identical layout and alignment to `AtomicU64`.
unsafe impl CmpxchgValue for u64 {
    /// # Safety
    ///
    /// See [`CmpxchgValue::cmpxchg`].
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self {
        // SAFETY: `pointer` is guaranteed by caller to point to a live, properly aligned `u64` atomic location.
        unsafe { AtomicU64::from_ptr(pointer) }
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap_or_else(|actual| actual)
    }
}

// SAFETY: `i32` has identical layout and alignment to `AtomicI32`.
unsafe impl CmpxchgValue for i32 {
    /// # Safety
    ///
    /// See [`CmpxchgValue::cmpxchg`].
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self {
        // SAFETY: `pointer` is guaranteed by caller to point to a live, properly aligned `i32` atomic location.
        unsafe { AtomicI32::from_ptr(pointer) }
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap_or_else(|actual| actual)
    }
}

// SAFETY: `i64` has identical layout and alignment to `AtomicI64`.
unsafe impl CmpxchgValue for i64 {
    /// # Safety
    ///
    /// See [`CmpxchgValue::cmpxchg`].
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self {
        // SAFETY: `pointer` is guaranteed by caller to point to a live, properly aligned `i64` atomic location.
        unsafe { AtomicI64::from_ptr(pointer) }
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap_or_else(|actual| actual)
    }
}

// SAFETY: `usize` has identical layout and alignment to `AtomicUsize`.
unsafe impl CmpxchgValue for usize {
    /// # Safety
    ///
    /// See [`CmpxchgValue::cmpxchg`].
    unsafe fn cmpxchg(pointer: *mut Self, old: Self, new: Self) -> Self {
        // SAFETY: `pointer` is guaranteed by caller to point to a live, properly aligned `usize` atomic location.
        unsafe { AtomicUsize::from_ptr(pointer) }
            .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
            .unwrap_or_else(|actual| actual)
    }
}

#[inline]
/// Atomically compare and replace a supported integer or pointer value.
///
/// # Safety
///
/// `pointer` must point to a live, properly aligned `T` value accessed only
/// atomically while this operation runs.
pub unsafe fn cmpxchg<T: CmpxchgValue>(pointer: *mut T, old: T, new: T) -> T {
    // SAFETY: Preconditions are forwarded directly to `T::cmpxchg`: `pointer` is live, properly aligned, and accessed only atomically.
    unsafe { T::cmpxchg(pointer, old, new) }
}

#[inline]
/// 64-bit specialization of [`cmpxchg`].
///
/// # Safety
///
/// `pointer` must point to a live, properly aligned `u64` accessed only
/// atomically while this operation runs.
pub unsafe fn cmpxchg64(pointer: *mut u64, old: u64, new: u64) -> u64 {
    // SAFETY: Preconditions are forwarded to `<u64 as CmpxchgValue>::cmpxchg`: `pointer` is live, properly aligned, and accessed only atomically.
    unsafe { <u64 as CmpxchgValue>::cmpxchg(pointer, old, new) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_bitops_match_linux_flag_semantics() {
        let mut flags = 0u64;
        assert!(!test_and_set_bit(35u32, &mut flags));
        assert!(test_bit(35u32, &flags));
        assert!(test_and_clear_bit(35u32, &mut flags));
        assert!(!test_bit(35u32, &flags));
    }

    #[test]
    fn bitfield_and_half_word_helpers_match_linux_macros() {
        assert_eq!(field_prep(0x0000_ff00, 0x42), 0x4200);
        assert!(field_fit(0x0000_ff00, 0xff));
        assert!(!field_fit(0x0000_ff00, 0x100));
        assert_eq!(order_base_2(1), 0);
        assert_eq!(order_base_2(17), 5);
        assert_eq!(circ_space(0, 0, 8), 7);
        // CIRC_SPACE(head=5, tail=2, 8) = (2 - 6) & 7; the producer is ahead of the consumer.
        assert_eq!(circ_space(5, 2, 8), 4);
        assert_eq!(circ_space(2, 5, 8), 2);
        assert_eq!(bit(8), 0x100);
        assert_eq!(lower_32_bits(0x1234_5678_9abc_def0u64), 0x9abc_def0);
        assert_eq!(upper_32_bits(0x1234_5678_9abc_def0u64), 0x1234_5678);
        assert!(is_aligned(0x4000usize, 0x1000usize));
    }
}
