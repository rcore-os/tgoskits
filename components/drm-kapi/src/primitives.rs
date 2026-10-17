// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Small architecture-neutral Linux primitives shared by the imported DRM
//! headers. These preserve their integer/ordering semantics; they do not
//! emulate device or GEM operations.

use core::{
    ffi::{c_char, c_int, c_long, c_ulong, c_void},
    sync::atomic::{AtomicI32, AtomicU64, Ordering, fence},
};

#[inline]
pub fn min_t<T: Ord>(a: T, b: T) -> T {
    core::cmp::min(a, b)
}

#[inline]
pub fn max_t<T: Ord>(a: T, b: T) -> T {
    core::cmp::max(a, b)
}

/// Linux `hweight32()`: population count of the low 32 bits.
#[inline]
pub const fn hweight32(value: u32) -> u32 {
    value.count_ones()
}

/// Linux `hweight8()`: population count of the low eight bits.
#[inline]
pub const fn hweight8(value: u8) -> u32 {
    value.count_ones()
}

/// Linux `hweight16()`: population count of the low sixteen bits.
#[inline]
pub const fn hweight16(value: u16) -> u32 {
    value.count_ones()
}

pub trait LinuxUnsigned: Copy + Ord {
    const ZERO: Self;
    const ONE: Self;
    fn wrapping_sub(self, rhs: Self) -> Self;
    fn wrapping_add(self, rhs: Self) -> Self;
    fn bit_or(self, rhs: Self) -> Self;
    fn bit_and(self, rhs: Self) -> Self;
    fn bit_not(self) -> Self;
    fn trailing_zeros(self) -> u32;
    fn to_u64(self) -> u64;
    fn from_u64(value: u64) -> Self;
}

#[macro_export]
macro_rules! impl_linux_unsigned {
    ($($t:ty),+ $(,)?) => {$ (
        impl LinuxUnsigned for $t {
            const ZERO: Self = 0;
            const ONE: Self = 1;
            fn wrapping_sub(self, rhs: Self) -> Self { self.wrapping_sub(rhs) }
            fn wrapping_add(self, rhs: Self) -> Self { self.wrapping_add(rhs) }
            fn bit_or(self, rhs: Self) -> Self { self | rhs }
            fn bit_and(self, rhs: Self) -> Self { self & rhs }
            fn bit_not(self) -> Self { !self }
            fn trailing_zeros(self) -> u32 { self.trailing_zeros() }
            fn to_u64(self) -> u64 { self as u64 }
            fn from_u64(value: u64) -> Self { value as Self }
        }
    )+};
}

impl_linux_unsigned!(u8, u16, u32, u64, usize);

/// Linux `round_up(x, y)`: rounds to the next `y`-aligned value; `y` is
/// required to be a nonzero power of two, as in the source macro contract.
#[inline]
pub fn round_up<T: LinuxUnsigned>(value: T, alignment: T) -> T {
    assert!(alignment > T::ZERO && alignment.bit_and(alignment.wrapping_sub(T::ONE)) == T::ZERO);
    value
        .wrapping_add(alignment.wrapping_sub(T::ONE))
        .bit_and(alignment.wrapping_sub(T::ONE).bit_not())
}

#[inline]
pub fn page_align<T: LinuxUnsigned>(value: T) -> T {
    round_up(value, T::from_u64(4096))
}

/// Linux `smp_store_mb()`: single-copy store followed by a full memory
/// barrier, as specified by the Linux memory model.
///
/// # Safety
/// `place` must point to a live, properly aligned writable `T` location and
/// must not be concurrently accessed non-atomically in a way that races.
#[inline]
pub unsafe fn smp_store_mb<T>(place: *mut T, value: T) {
    // SAFETY: `place` is valid, properly aligned, and writable for `T` without racing non-atomic accesses per caller contract.
    unsafe { core::ptr::write_volatile(place, value) };
    fence(Ordering::SeqCst);
}

/// `is_power_of_2()` from include/linux/log2.h.
#[inline]
pub fn is_power_of_2<T: LinuxUnsigned>(value: T) -> bool {
    value > T::ZERO && value.bit_and(value.wrapping_sub(T::ONE)) == T::ZERO
}

#[inline]
pub fn offset_in_page<T: LinuxUnsigned + PageMask>(value: T) -> T {
    value.bit_and(T::from_page_mask().bit_not())
}

/// `memchr_inv()` from lib/string.c: return the first byte not equal to `c`.
///
/// # Safety
/// When `bytes` is nonzero, `start` must point to at least that many readable
/// bytes.
#[inline]
pub unsafe fn memchr_inv(start: *const c_void, c: i32, bytes: usize) -> *mut c_void {
    let start = start.cast::<u8>();
    for i in 0..bytes {
        // SAFETY: i < bytes, and caller guarantees start points to at least bytes readable bytes.
        if unsafe { *start.add(i) } != c as u8 {
            // SAFETY: start.add(i) is within the caller-guaranteed readable range and cast back to mut pointer.
            return unsafe { start.add(i).cast_mut().cast() };
        }
    }
    core::ptr::null_mut()
}

pub trait PageMask: LinuxUnsigned {
    fn from_page_mask() -> Self;
}
#[macro_export]
macro_rules! impl_page_mask {
    ($($t:ty),+ $(,)?) => {$ (
        impl PageMask for $t {
            fn from_page_mask() -> Self { !((4096usize - 1) as $t) }
        }
    )+};
}
impl_page_mask!(u8, u16, u32, u64, usize);

/// Linux memory barriers used around device-visible state publication.
#[inline]
pub fn wmb() {
    fence(Ordering::Release);
}

#[inline]
pub fn rmb() {
    fence(Ordering::Acquire);
}

#[inline]
pub fn mb() {
    fence(Ordering::SeqCst);
}

#[inline]
pub fn is_power_of_2_u64(value: u64) -> bool {
    is_power_of_2(value)
}

pub const INT_MIN: i32 = i32::MIN;

/// Atomic `xchg(ptr, 0)` for Linux's pointer fetch-and-zero call sites.
pub trait AtomicZero: Copy {
    /// # Safety
    ///
    /// `place` must be valid, naturally aligned, initialized pointer storage
    /// whose concurrent accesses are all atomic.
    unsafe fn swap_zero(place: *mut Self) -> Self;
}

impl<T> AtomicZero for *mut T {
    unsafe fn swap_zero(place: *mut Self) -> Self {
        // SAFETY: the caller guarantees valid, aligned pointer storage that is
        // only accessed atomically.
        unsafe { core::sync::atomic::AtomicPtr::from_ptr(place) }
            .swap(core::ptr::null_mut(), Ordering::AcqRel)
    }
}

impl<T> AtomicZero for *const T {
    unsafe fn swap_zero(place: *mut Self) -> Self {
        // SAFETY: same pointer-sized storage contract as the mutable form.
        unsafe { core::sync::atomic::AtomicPtr::from_ptr(place.cast::<*mut T>()) }
            .swap(core::ptr::null_mut(), Ordering::AcqRel)
    }
}

#[inline]
pub fn fetch_and_zero<T: AtomicZero>(place: &mut T) -> T {
    // SAFETY: a unique reference is valid, aligned, initialized storage.
    unsafe { T::swap_zero(place) }
}

/// Linux jiffies derived from the kernel monotonic clock and configured HZ.
#[inline]
pub fn jiffies() -> u64 {
    let nanos = crate::clock::now_ns();
    let second = crate::clock::NANOS_PER_SECOND;
    let hz = crate::config::CONFIG_HZ as u64;
    (nanos / second) * hz + (nanos % second) * hz / second
}

/// Linux `round_jiffies_up_relative()` policy: align a relative deadline to
/// the next second boundary. Rounding is only retained when the wrapped
/// absolute deadline is still in the future. Linux's per-CPU skew is omitted:
/// the portable clock provider intentionally has no CPU-identity dependency.
pub fn round_jiffies_up_relative(delta: u64) -> u64 {
    let now = jiffies();
    let hz = crate::config::CONFIG_HZ as u64;
    let target = now.wrapping_add(delta);
    let rem = target % hz;
    let rounded = target.wrapping_sub(rem).wrapping_add(hz);
    let relative = rounded.wrapping_sub(now);
    if relative as i64 > 0 { relative } else { delta }
}

#[inline]
pub fn jiffies_to_msecs<T: LinuxUnsigned>(ticks: T) -> u64 {
    ticks.to_u64().saturating_mul(1000) / crate::config::CONFIG_HZ as u64
}

/// Linux `udelay()` through the installed platform clock provider.
#[inline]
pub fn udelay(micros: u32) {
    crate::clock::delay_us(micros);
}

/// Linux `mdelay()`: millisecond busy wait.
#[inline]
pub fn mdelay(millis: u32) {
    crate::clock::delay_us(millis.saturating_mul(1000));
}

/// Linux `ndelay()`: nanosecond busy wait.
#[inline]
pub fn ndelay(nanos: u32) {
    crate::clock::delay_us(nanos.saturating_add(999) / 1000);
}

/// Linux `ktime_to_ms()` for the nanosecond `ktime_t` representation.
#[inline]
pub const fn ktime_to_ms(nanoseconds: i64) -> i64 {
    nanoseconds / 1_000_000
}

/// Linux `ktime_to_us()`.
#[inline]
pub const fn ktime_to_us(nanoseconds: i64) -> i64 {
    nanoseconds / 1_000
}

/// Linux `ktime_to_ns()`.
#[inline]
pub const fn ktime_to_ns(nanoseconds: i64) -> i64 {
    nanoseconds
}

/// Linux `DIV_ROUND_CLOSEST_ULL(n, d)` for unsigned 64-bit quantities.
#[inline]
pub const fn div_round_closest_ull(numerator: u64, denominator: u64) -> u64 {
    numerator.wrapping_add(denominator / 2) / denominator
}

/// Linux `hex_dump_to_buffer()` for the native-endian grouped words used by
/// the DRM error-state hexdump. The caller supplies a sufficiently large line
/// buffer, as the upstream call does (128 bytes for at most 32 input bytes).
///
/// # Safety
/// `buf` must point to `len` readable bytes and, when `linebuflen` is nonzero,
/// `linebuf` must point to that many writable bytes.
pub unsafe fn hex_dump_to_buffer(
    buf: *const c_void,
    len: usize,
    rowsize: usize,
    groupsize: usize,
    linebuf: *mut c_char,
    linebuflen: usize,
    ascii: bool,
) -> usize {
    if buf.is_null() || linebuf.is_null() || linebuflen == 0 {
        return 0;
    }
    let rowsize = if rowsize == 16 || rowsize == 32 {
        rowsize
    } else {
        16
    };
    let len = core::cmp::min(len, rowsize);
    let mut groupsize = groupsize;
    if !groupsize.is_power_of_two() || groupsize > 8 || len % groupsize != 0 {
        groupsize = 1;
    }

    let mut written = 0usize;
    macro_rules! put {
        ($byte:expr) => {{
            if written + 1 < linebuflen {
                // SAFETY: written + 1 < linebuflen, and linebuf is non-null with linebuflen writable bytes.
                unsafe { *linebuf.add(written) = $byte as c_char };
            }
            written += 1;
        }};
    }

    let bytes = buf.cast::<u8>();
    let digits = groupsize * 2;
    let mut pos = 0;
    while pos < len {
        let count = core::cmp::min(groupsize, len - pos);
        let mut word = 0u64;
        for i in 0..count {
            // SAFETY: pos + i < len, and buf points to at least len readable bytes.
            word |= (unsafe { *bytes.add(pos + i) } as u64) << (8 * i);
        }
        for shift in (0..digits).rev() {
            let nibble = ((word >> (shift * 4)) & 0xf) as u8;
            put!(if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            });
        }
        put!(b' ');
        pos += count;
    }

    if ascii {
        let ascii_column = rowsize * 2 + rowsize / groupsize + 1;
        while written < ascii_column {
            put!(b' ');
        }
        for i in 0..len {
            // SAFETY: i < len, and buf points to at least len readable bytes.
            let byte = unsafe { *bytes.add(i) };
            put!(if byte.is_ascii_graphic() || byte == b' ' {
                byte
            } else {
                b'.'
            });
        }
    }
    // SAFETY: written offset clamped to linebuflen - 1 is within linebuf bounds; linebuflen > 0.
    unsafe { *linebuf.add(core::cmp::min(written, linebuflen - 1)) = 0 };
    written
}

#[inline]
pub fn jiffies_to_nsecs<T: LinuxUnsigned>(ticks: T) -> u64 {
    ticks
        .to_u64()
        .saturating_mul(crate::clock::NANOS_PER_SECOND)
        / crate::config::CONFIG_HZ as u64
}

#[inline]
pub fn msecs_to_jiffies<T: LinuxUnsigned>(milliseconds: T) -> u64 {
    let hz = crate::config::CONFIG_HZ as u64;
    milliseconds.to_u64().saturating_mul(hz).saturating_add(999) / 1000
}

#[inline]
pub fn overflows_u32<T: LinuxUnsigned>(value: T) -> bool {
    value.to_u64() > u32::MAX as u64
}

pub trait LinuxBitScan: Copy {
    fn scan_word(self) -> u64;
}
impl LinuxBitScan for u32 {
    fn scan_word(self) -> u64 {
        self as u64
    }
}
impl LinuxBitScan for u64 {
    fn scan_word(self) -> u64 {
        self
    }
}
impl LinuxBitScan for usize {
    fn scan_word(self) -> u64 {
        self as u64
    }
}
impl LinuxBitScan for i32 {
    fn scan_word(self) -> u64 {
        self as u32 as u64
    }
}

/// Linux `__ffs`: zero-based index of the first set bit (input must be nonzero).
#[inline]
pub fn __ffs<T: LinuxBitScan>(value: T) -> u32 {
    let value = value.scan_word();
    assert_ne!(value, 0, "Linux __ffs requires a nonzero word");
    value.trailing_zeros()
}

/// Linux `ffs`: one-based index, with zero mapping to zero.
#[inline]
pub fn ffs<T: LinuxBitScan>(value: T) -> i32 {
    let value = value.scan_word();
    if value == 0 {
        0
    } else {
        value.trailing_zeros() as i32 + 1
    }
}

#[inline]
pub fn ilog2<T: LinuxBitScan>(value: T) -> u32 {
    let value = value.scan_word();
    assert_ne!(value, 0, "Linux ilog2 requires a nonzero input");
    u64::BITS - 1 - value.leading_zeros()
}

#[inline]
pub fn max<T: Ord>(left: T, right: T) -> T {
    core::cmp::max(left, right)
}

#[inline]
pub fn ktime_get() -> i64 {
    crate::clock::now_ns() as i64
}

#[inline]
pub fn ktime_get_boottime() -> i64 {
    ktime_get()
}

#[inline]
pub fn ktime_get_raw() -> i64 {
    ktime_get()
}

/// Linux `ktime_get_raw_fast_ns()` binding. The configured kernel time
/// source is a hardware-backed monotonic counter with no wall-clock/NTP
/// adjustment, matching the raw monotonic domain required by the driver's statistics.
#[inline]
pub fn ktime_get_raw_fast_ns() -> u64 {
    crate::clock::now_ns()
}

#[inline]
pub const fn ktime_set(secs: i64, nsecs: c_long) -> i64 {
    secs.saturating_mul(1_000_000_000).saturating_add(nsecs)
}

#[inline]
pub const fn ktime_sub(lhs: i64, rhs: i64) -> i64 {
    lhs - rhs
}

#[inline]
pub const fn ktime_add(lhs: i64, rhs: i64) -> i64 {
    lhs.saturating_add(rhs)
}

#[inline]
pub const fn ktime_add_ms(kt: i64, ms: i64) -> i64 {
    kt.saturating_add(ms.saturating_mul(1_000_000))
}

#[inline]
pub const fn ktime_add_us(kt: i64, us: i64) -> i64 {
    kt.saturating_add(us.saturating_mul(1_000))
}

#[inline]
pub const fn ktime_after(cmp1: i64, cmp2: i64) -> bool {
    cmp1 > cmp2
}

#[inline]
pub const fn ktime_before(cmp1: i64, cmp2: i64) -> bool {
    cmp1 < cmp2
}

#[inline]
pub const fn ktime_ms_delta(later: i64, earlier: i64) -> i64 {
    ktime_to_ms(ktime_sub(later, earlier))
}

#[inline]
pub const fn ktime_us_delta(later: i64, earlier: i64) -> i64 {
    ktime_to_us(ktime_sub(later, earlier))
}

/// `ARRAY_SIZE(array)` for fixed C-layout Rust arrays.
#[allow(non_snake_case)]
#[inline]
pub fn ARRAY_SIZE<T: Copy, const N: usize>(_array: [T; N]) -> usize {
    N
}

#[inline]
pub const fn str_yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

pub trait AsConstBytePointer {
    fn into_const_byte_pointer(self) -> *const u8;
}
pub trait AsMutBytePointer {
    fn into_mut_byte_pointer(self) -> *mut u8;
}
impl<T> AsConstBytePointer for *const T {
    fn into_const_byte_pointer(self) -> *const u8 {
        self.cast()
    }
}
impl<T> AsConstBytePointer for *mut T {
    fn into_const_byte_pointer(self) -> *const u8 {
        self.cast()
    }
}
impl<T> AsMutBytePointer for *mut T {
    fn into_mut_byte_pointer(self) -> *mut u8 {
        self.cast()
    }
}

/// Linux `memcpy` for the imported raw-pointer call sites.
///
/// # Safety
/// For nonzero `size`, `source` must be readable and `destination` writable
/// for `size` bytes, and the ranges must not overlap.
pub unsafe fn memcpy<D: AsMutBytePointer, S: AsConstBytePointer>(
    destination: D,
    source: S,
    size: usize,
) -> *mut c_void {
    let destination = destination.into_mut_byte_pointer();
    let source = source.into_const_byte_pointer();
    if size != 0 {
        assert!(!destination.is_null() && !source.is_null());
        // SAFETY: Linux memcpy call sites provide valid source/destination
        // ranges of `size` bytes and require non-overlapping storage.
        unsafe { core::ptr::copy_nonoverlapping(source, destination, size) };
    }
    destination.cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_round_up_and_power_of_two_follow_linux_rules() {
        assert_eq!(round_up(4097u32, 4096), 8192);
        assert_eq!(round_up(0usize, 4096), 0);
        assert!(is_power_of_2(1u32));
        assert!(is_power_of_2(4096usize));
        assert!(!is_power_of_2(0u32));
        assert!(!is_power_of_2(3u64));
        assert_eq!(offset_in_page(0x1234usize), 0x234);
    }

    #[test]
    fn test_time_comparisons() {
        let now: c_ulong = 1000;
        let past: c_ulong = 900;
        let future: c_ulong = 1100;
        assert!(time_after(now, past));
        assert!(!time_after(now, future));
        assert!(time_before(past, now));
        assert!(!time_before(future, now));
        assert!(time_after_eq(now, past));
        assert!(time_after_eq(now, now));
        assert!(!time_after_eq(now, future));
        assert!(time_before_eq(past, now));
        assert!(time_before_eq(now, now));
        assert!(!time_before_eq(future, now));
        assert!(time_in_range(now, past, future));
        assert!(!time_in_range(past, now, future));

        // Wrap-around across unsigned boundary
        let wrap_past: c_ulong = c_ulong::MAX - 10;
        let wrap_now: c_ulong = 10;
        assert!(time_after(wrap_now, wrap_past));
        assert!(time_before(wrap_past, wrap_now));
    }

    #[test]
    fn test_ktime_helpers() {
        let kt = ktime_set(2, 500_000_000);
        assert_eq!(kt, 2_500_000_000);
        assert_eq!(ktime_to_ms(kt), 2500);
        assert_eq!(ktime_to_us(kt), 2_500_000);
        assert_eq!(ktime_to_ns(kt), 2_500_000_000);

        let later = ktime_add_ms(kt, 100);
        assert_eq!(later, 2_600_000_000);
        assert!(ktime_after(later, kt));
        assert!(ktime_before(kt, later));
        assert_eq!(ktime_ms_delta(later, kt), 100);
        assert_eq!(ktime_us_delta(later, kt), 100_000);

        let added_us = ktime_add_us(kt, 50);
        assert_eq!(added_us, 2_500_050_000);
        assert_eq!(ktime_sub(added_us, kt), 50_000);
    }

    #[test]
    fn test_jiffies_helpers() {
        let j = usecs_to_jiffies(10_000); // 10ms at HZ=100 -> 1 tick
        assert_eq!(j, 1);
        let j_to = msecs_to_jiffies_timeout(10);
        assert!(j_to >= 1);
    }
}

/// Linux jiffies `time_after(a, b)` wrap-safe comparison.
#[inline]
pub const fn time_after(a: c_ulong, b: c_ulong) -> bool {
    (b.wrapping_sub(a) as c_long) < 0
}

/// Linux jiffies `time_before(a, b)` wrap-safe comparison.
#[inline]
pub const fn time_before(a: c_ulong, b: c_ulong) -> bool {
    time_after(b, a)
}

/// Linux jiffies `time_after_eq(a, b)` wrap-safe comparison.
#[inline]
pub const fn time_after_eq(a: c_ulong, b: c_ulong) -> bool {
    (a.wrapping_sub(b) as c_long) >= 0
}

/// Linux jiffies `time_before_eq(a, b)` wrap-safe comparison.
#[inline]
pub const fn time_before_eq(a: c_ulong, b: c_ulong) -> bool {
    time_after_eq(b, a)
}

/// Linux jiffies `time_in_range(a, b, c)` wrap-safe check whether `a` is in `[b, c]`.
#[inline]
pub const fn time_in_range(a: c_ulong, b: c_ulong, c: c_ulong) -> bool {
    time_after_eq(a, b) && time_before_eq(a, c)
}

/// Linux `usecs_to_jiffies()`.
#[inline]
pub fn usecs_to_jiffies(usecs: u32) -> c_ulong {
    let hz = crate::config::CONFIG_HZ as u64;
    ((usecs as u64).saturating_mul(hz).saturating_add(999_999) / 1_000_000) as c_ulong
}

/// Linux `msecs_to_jiffies_timeout()`.
#[inline]
pub fn msecs_to_jiffies_timeout(milliseconds: u32) -> c_ulong {
    let j = msecs_to_jiffies(milliseconds);
    min_t(j + 1, crate::config::MAX_SCHEDULE_TIMEOUT) as c_ulong
}

/// Linux `wait_remaining_ms_from_jiffies()`.
#[inline]
pub fn wait_remaining_ms_from_jiffies(timestamp_jiffies: u64, to_wait_ms: i32) {
    let tmp_jiffies = jiffies();
    let target_jiffies =
        timestamp_jiffies.wrapping_add(msecs_to_jiffies_timeout(to_wait_ms.max(0) as u32));
    if time_after(target_jiffies as c_ulong, tmp_jiffies as c_ulong) {
        let mut remaining_jiffies = target_jiffies.wrapping_sub(tmp_jiffies) as i64;
        while remaining_jiffies > 0 {
            // SAFETY: schedule_timeout_uninterruptible puts current task to sleep for remaining_jiffies ticks without external state mutation.
            remaining_jiffies =
                unsafe { crate::wait::schedule_timeout_uninterruptible(remaining_jiffies) };
        }
    }
}

// ---------------------------------------------------------------------------
// kref and the uniform random helper used by the GT scheduler.
// ---------------------------------------------------------------------------

/// Linux `kref_get()`: `refcount_inc()` on a live object.
///
/// # Safety
/// `kref` must point to a live `struct kref`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kref_get(kref: *mut crate::types::Kref) {
    assert!(!kref.is_null());
    // SAFETY: kref is non-null, aligned, points to a live struct kref whose counter field layout matches AtomicI32.
    let counter =
        unsafe { &*core::ptr::addr_of!((*kref).refcount.refs.counter).cast::<AtomicI32>() };
    let previous = counter.fetch_add(1, Ordering::Relaxed);
    assert!(previous > 0, "kref_get() on a released object");
}

/// Linux `kref_put()`: drops one reference and runs `release` when it was the
/// last one. Returns true when `release` ran.
///
/// # Safety
/// `kref` must point to a live `struct kref` owned by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kref_put(
    kref: *mut crate::types::Kref,
    release: Option<unsafe extern "C" fn(*mut crate::types::Kref)>,
) -> bool {
    assert!(!kref.is_null());
    // SAFETY: kref is non-null, aligned, points to a live struct kref whose counter field layout matches AtomicI32.
    let counter =
        unsafe { &*core::ptr::addr_of!((*kref).refcount.refs.counter).cast::<AtomicI32>() };
    let previous = counter.fetch_sub(1, Ordering::AcqRel);
    assert!(previous > 0, "kref_put() on a released object");
    if previous != 1 {
        return false;
    }
    let release = release.expect("kref_put() reached zero without a release function");
    // SAFETY: the refcount reached zero and kref is valid; calling caller-supplied release callback to reclaim the object.
    unsafe { release(kref) };
    true
}

static RANDOM_STATE: AtomicU64 = AtomicU64::new(0);

/// Non-cryptographic 32-bit random value (xorshift64*, seeded from the host
/// monotonic clock when no x86 cycle counter is available).
fn random_u32() -> u32 {
    let mut state = RANDOM_STATE.load(Ordering::Relaxed);
    if state == 0 {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: RDTSC has no memory effects and is always available on x86_64.
            state = unsafe { core::arch::x86_64::_rdtsc() } | 1;
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            state = crate::clock::now_ns() | 1;
        }
    }
    state ^= state >> 12;
    state ^= state << 25;
    state ^= state >> 27;
    RANDOM_STATE.store(state, Ordering::Relaxed);
    (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
}

/// Linux `get_random_u32_below()`: uniform in `[0, range)`, using the same
/// multiply-and-reject construction as the kernel.
#[unsafe(no_mangle)]
pub extern "C" fn get_random_u32_below(range: u32) -> u32 {
    let mut mult = u64::from(range) * u64::from(random_u32());
    if (mult as u32) < range {
        let bound = range.wrapping_neg() % range;
        while (mult as u32) < bound {
            mult = u64::from(range) * u64::from(random_u32());
        }
    }
    (mult >> 32) as u32
}

// ---------------------------------------------------------------------------
// Debug-object, ref-tracker and stack-depot hooks in the reference configuration.
// ---------------------------------------------------------------------------

/// Linux `debug_object_init()`. The reference configuration has
/// CONFIG_DEBUG_OBJECTS unset, where debugobjects.h defines this as an empty
/// inline function.
#[unsafe(no_mangle)]
pub extern "C" fn debug_object_init(_addr: *mut c_void, _desc: *const c_void) {}

/// Linux `debug_object_activate()`; empty with CONFIG_DEBUG_OBJECTS unset.
#[unsafe(no_mangle)]
pub extern "C" fn debug_object_activate(_addr: *mut c_void, _desc: *const c_void) {}

/// Linux `debug_object_deactivate()`; empty with CONFIG_DEBUG_OBJECTS unset.
#[unsafe(no_mangle)]
pub extern "C" fn debug_object_deactivate(_addr: *mut c_void, _desc: *const c_void) {}

/// Linux `debug_object_assert_init()`; empty with CONFIG_DEBUG_OBJECTS unset.
#[unsafe(no_mangle)]
pub extern "C" fn debug_object_assert_init(_addr: *mut c_void, _desc: *const c_void) {}

/// Linux `debug_object_free()`; empty with CONFIG_DEBUG_OBJECTS unset.
#[unsafe(no_mangle)]
pub extern "C" fn debug_object_free(_addr: *mut c_void, _desc: *const c_void) {}

/// Linux `ref_tracker_alloc()`. The reference configuration has CONFIG_REF_TRACKER
/// unset, where ref_tracker.h defines this as `return 0`.
#[unsafe(no_mangle)]
pub extern "C" fn ref_tracker_alloc(
    _dir: *mut c_void,
    _trackerp: *mut *mut c_void,
    _gfp: u32,
) -> i32 {
    0
}

/// Linux `ref_tracker_free()`; `return 0` with CONFIG_REF_TRACKER unset.
#[unsafe(no_mangle)]
pub extern "C" fn ref_tracker_free(_dir: *mut c_void, _trackerp: *mut *mut c_void) -> i32 {
    0
}

/// Linux `stack_depot_snprint()`. Stack handles come from `stack_depot_save()`,
/// which has no producer in this kernel, so every handle is 0 and names an
/// empty trace. The output is an empty, NUL-terminated string as Linux prints
/// for an empty trace.
///
/// # Safety
/// When `size` is nonzero, `buf` must point to that many writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stack_depot_snprint(
    _handle: u32,
    buf: *mut c_char,
    size: usize,
    _spaces: u32,
) -> usize {
    if size > 0 && !buf.is_null() {
        // SAFETY: size > 0 and buf is non-null; caller guarantees buf points to at least size writable bytes.
        unsafe { *buf = 0 };
    }
    0
}

/// Linux `atomic_notifier_call_chain()`: call each notifier in priority order
/// until one returns `NOTIFY_STOP_MASK`, and return the last result. `head`
/// is a `struct atomic_notifier_head` whose chain pointer follows its lock.
///
/// # Safety
/// `head` must point to a live initialized notifier head and each linked block
/// and callback must remain valid while the chain is traversed. Callback data
/// must satisfy those callbacks' contracts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn atomic_notifier_call_chain(
    head: *mut c_void,
    value: c_ulong,
    data: *mut c_void,
) -> c_int {
    use crate::{gem_memory::NotifierBlock, types::AtomicNotifierHead};
    const NOTIFY_DONE: c_int = 0;
    const NOTIFY_STOP_MASK: c_int = 0x8000;
    assert!(!head.is_null());
    let mut ret = NOTIFY_DONE;
    // SAFETY: head is non-null, aligned, and points to a live AtomicNotifierHead.
    let mut block = unsafe {
        (*head.cast::<AtomicNotifierHead>())
            .head
            .cast::<NotifierBlock>()
    };
    while !block.is_null() {
        // SAFETY: block is non-null and points to a valid NotifierBlock in the chain.
        let call = unsafe { (*block).notifier_call }
            .expect("notifier block without a notifier_call function");
        // SAFETY: calling notifier_call callback with valid block, value, and data parameters per caller contract.
        ret = unsafe { call(block, value, data) };
        if ret & NOTIFY_STOP_MASK != 0 {
            break;
        }
        // SAFETY: block is valid; traversing to next NotifierBlock in the chain.
        block = unsafe { (*block).next };
    }
    ret
}

/// Linux `memcpy_fromio()`: copy from device memory one volatile byte at a
/// time, so each read reaches the device exactly once.
///
/// # Safety
/// `dst` must be writable and `src` a readable MMIO range of `len` bytes; the
/// device mapping must remain live for the duration of the copy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy_fromio(dst: *mut c_void, src: *const c_void, len: usize) {
    let dst = dst.cast::<u8>();
    let src = src.cast::<u8>();
    for offset in 0..len {
        // SAFETY: offset < len; caller guarantees dst is writable and src is a readable MMIO range of len bytes.
        unsafe { *dst.add(offset) = core::ptr::read_volatile(src.add(offset)) };
    }
}

/// Kernel provider for uncached MMIO mappings, installed by the kernel's
/// memory-management side. `ioremap` returns NULL when the mapping cannot be
/// made; `iounmap` releases a mapping returned by `ioremap`.
#[repr(C)]
pub struct IoremapProvider {
    /// Maps a physical MMIO address range to virtual address space.
    ///
    /// # Safety
    /// `phys..phys + size` must specify a valid reserved physical MMIO range.
    pub ioremap: unsafe extern "C" fn(phys: u64, size: u64) -> *mut c_void,
    /// Unmaps a virtual address range previously mapped by `ioremap`.
    ///
    /// # Safety
    /// `addr` must be a valid mapping returned by `ioremap` and must not be used after unmapping.
    pub iounmap: unsafe extern "C" fn(addr: *mut c_void),
}

static IOREMAP_PROVIDER: core::sync::atomic::AtomicPtr<IoremapProvider> =
    core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Install the MMIO mapping provider. It can be installed once.
pub fn install_ioremap_provider(provider: &'static IoremapProvider) -> Result<(), &'static str> {
    IOREMAP_PROVIDER
        .compare_exchange(
            core::ptr::null_mut(),
            core::ptr::from_ref(provider).cast_mut(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .map(|_| ())
        .map_err(|_| "ioremap provider already installed")
}

fn ioremap_provider() -> Option<&'static IoremapProvider> {
    let provider = IOREMAP_PROVIDER.load(Ordering::Acquire);
    if provider.is_null() {
        None
    } else {
        // SAFETY: only `install_ioremap_provider` stores a non-null pointer, and it
        // stores a `'static` reference.
        Some(unsafe { &*provider })
    }
}

/// Linux `ioremap()`: an uncached kernel mapping of physical `base`. Returns
/// NULL when no provider is installed, which callers already treat as failure.
///
/// # Safety
/// `base..base + size` must denote a reserved physical MMIO range permitted to
/// the installed provider; the caller must avoid conflicting aliases.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ioremap(base: u64, size: u64) -> *mut c_void {
    match ioremap_provider() {
        // SAFETY: base..base+size is a valid MMIO range; invoking the registered kernel MMIO mapping provider.
        Some(provider) => unsafe { (provider.ioremap)(base, size) },
        None => core::ptr::null_mut(),
    }
}

/// Linux `iounmap()`. A mapping cannot exist without a provider, so reaching
/// this without one is an invariant violation.
///
/// # Safety
/// `addr` must be a live mapping returned by the installed `ioremap` provider
/// and must not be used after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn iounmap(addr: *mut c_void) {
    let provider = ioremap_provider().expect("iounmap() without an ioremap provider");
    // SAFETY: addr is a live MMIO mapping created by the installed provider, now released per caller contract.
    unsafe { (provider.iounmap)(addr) };
}
