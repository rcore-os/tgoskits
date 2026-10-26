// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux XArray surface used by DRM lookup tables.
//!
//! The outer `struct xarray` storage follows include/linux/xarray.h; this Rust
//! backend stores the indexed entries in an ordered map behind `xa_head` and
//! preserves the public load/store/erase/allocation and lock contracts.

use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use core::{
    ffi::{c_ulong, c_void},
    mem::offset_of,
};

use crate::{
    locks::{
        spin_lock, spin_lock_irq, spin_lock_irqsave, spin_unlock, spin_unlock_irq,
        spin_unlock_irqrestore,
    },
    types::Spinlock,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XArray {
    pub xa_lock: Spinlock,
    pub xa_flags: u32,
    pub xa_head: *mut c_void,
}

impl XArray {
    pub const fn new(flags: u32) -> Self {
        Self {
            xa_lock: Spinlock::new(),
            xa_flags: flags,
            xa_head: core::ptr::null_mut(),
        }
    }
}

type XArrayHeader = XArray;

#[derive(Default)]
struct XaEntries {
    entries: BTreeMap<u32, *mut c_void>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct XaLimit {
    pub min: u32,
    pub max: u32,
}

pub const XA_FLAGS_ALLOC: u32 = 1 << 1;
pub const XA_FLAGS_LOCK_IRQ: u32 = 1 << 2;
pub const XA_LIMIT_32B: XaLimit = XaLimit {
    min: 0,
    max: u32::MAX,
};

#[inline]
fn header<T>(array: *mut T) -> *mut XArrayHeader {
    array.cast()
}

/// # Safety
///
/// `array` must point to a live, properly aligned memory region whose prefix
/// matches `XArrayHeader` (`XArray`), and must not be concurrently mutated without
/// holding the corresponding lock.
unsafe fn entries<T>(array: *mut T, allocate: bool) -> *mut XaEntries {
    // SAFETY: Per function safety contract, `array` starts with an `XArrayHeader`.
    // Reading/writing `xa_head` and allocating a new `XaEntries` if null and requested
    // preserves alignment and valid ownership.
    unsafe {
        let head = core::ptr::addr_of_mut!((*header(array)).xa_head);
        if (*head).is_null() && allocate {
            *head = Box::into_raw(Box::new(XaEntries::default())).cast();
        }
        (*head).cast()
    }
}

pub fn xa_init_flags<T>(array: &mut T, flags: u32) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`.
    // Mutating `xa_flags` and allocating `xa_head` via `entries` is exclusive and safe.
    unsafe {
        (*header(array)).xa_flags = flags;
        let _ = entries(array, true);
    }
}

pub fn xa_destroy<T>(array: &mut T) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`.
    // `xa_head` was allocated via `Box::into_raw` in `entries`, so reconstructing the `Box`
    // safely reclaims the heap memory and clearing it prevents double-free.
    unsafe {
        let head = core::ptr::addr_of_mut!((*header(array)).xa_head);
        if !(*head).is_null() {
            drop(Box::from_raw((*head).cast::<XaEntries>()));
            *head = core::ptr::null_mut();
        }
    }
}

pub fn xa_lock<T>(array: &mut T) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`,
    // containing an aligned, initialized `xa_lock`.
    unsafe { spin_lock(&mut (*header(array)).xa_lock) }
}

pub fn xa_unlock<T>(array: &mut T) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`,
    // containing an aligned, initialized `xa_lock`.
    unsafe { spin_unlock(&mut (*header(array)).xa_lock) }
}

pub fn xa_lock_irq<T>(array: &mut T) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`,
    // containing an aligned, initialized `xa_lock`.
    unsafe { spin_lock_irq(&mut (*header(array)).xa_lock) }
}

pub fn xa_unlock_irq<T>(array: &mut T) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`,
    // containing an aligned, initialized `xa_lock`.
    unsafe { spin_unlock_irq(&mut (*header(array)).xa_lock) }
}

pub fn xa_lock_irqsave<T>(array: &mut T, flags: &mut u64) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`,
    // containing an aligned, initialized `xa_lock`.
    unsafe { spin_lock_irqsave(&mut (*header(array)).xa_lock, flags) }
}

pub fn xa_unlock_irqrestore<T>(array: &mut T, flags: u64) {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`,
    // containing an aligned, initialized `xa_lock`.
    unsafe { spin_unlock_irqrestore(&mut (*header(array)).xa_lock, flags) }
}

pub fn xa_load<T, V>(array: &mut T, index: u32) -> *mut V {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`.
    // `entries` safely retrieves the pointer to `XaEntries` without modifying ownership.
    unsafe {
        let head = entries(array, false);
        if head.is_null() {
            return core::ptr::null_mut();
        }
        (*head)
            .entries
            .get(&index)
            .copied()
            .unwrap_or_default()
            .cast()
    }
}

pub fn __xa_store<T, V>(array: &mut T, index: u32, value: *mut V, _flags: u32) -> *mut V {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`.
    // `entries(array, true)` ensures a valid `XaEntries` map is present and exclusively accessed.
    unsafe {
        let head = entries(array, true);
        (*head)
            .entries
            .insert(index, value.cast())
            .unwrap_or_default()
            .cast()
    }
}

pub fn __xa_erase<T, V>(array: &mut T, index: u32) -> *mut V {
    // SAFETY: `array` is an exclusive reference whose memory begins with `XArrayHeader`.
    // `entries(array, false)` accesses the existing map if present.
    unsafe {
        let head = entries(array, false);
        if head.is_null() {
            return core::ptr::null_mut();
        }
        (*head).entries.remove(&index).unwrap_or_default().cast()
    }
}

pub fn xa_erase_irq<T, V>(array: &mut T, index: u32) -> *mut V {
    let mut flags = 0u64;
    xa_lock_irqsave(array, &mut flags);
    let entry = __xa_erase(array, index);
    xa_unlock_irqrestore(array, flags);
    entry
}

pub fn xa_alloc_cyclic_irq<T, V>(
    array: &mut T,
    index: &mut u32,
    value: *mut V,
    limit: XaLimit,
    next: &mut u32,
    gfp: u32,
) -> i32 {
    if gfp & (1 << 10) == 0 {
        // The Rust heap has no nonblocking allocator API, so preserve Linux's
        // failure contract rather than allocating while IRQs may be disabled.
        return -12;
    }
    if limit.min > limit.max {
        return -22;
    }
    let mut flags = 0u64;
    xa_lock_irqsave(array, &mut flags);
    // SAFETY: `array` is an exclusive reference under spinlock protection.
    // `entries(array, true)` guarantees an allocated `XaEntries` map is returned and exclusively accessed.
    unsafe {
        let head = entries(array, true);
        let span = u64::from(limit.max) - u64::from(limit.min) + 1;
        let start = (*next).clamp(limit.min, limit.max);
        let mut candidate = start;
        let mut found = false;
        for _ in 0..span {
            if !(*head).entries.contains_key(&candidate) {
                found = true;
                break;
            }
            candidate = if candidate == limit.max {
                limit.min
            } else {
                candidate + 1
            };
        }
        if !found {
            xa_unlock_irqrestore(array, flags);
            return -12;
        }
        (*head).entries.insert(candidate, value.cast());
        *index = candidate;
        *next = if candidate == limit.max {
            limit.min
        } else {
            candidate + 1
        };
    }
    xa_unlock_irqrestore(array, flags);
    0
}

pub fn xa_snapshot<T>(array: &mut T) -> Vec<(u32, *mut c_void)> {
    // SAFETY: `array` is an exclusive reference whose memory starts with `XArrayHeader`.
    // It safely queries existing entries from the map into a freshly allocated Vec.
    unsafe {
        let head = entries(array, false);
        if head.is_null() {
            return Vec::new();
        }
        (*head)
            .entries
            .iter()
            .map(|(index, pointer)| (*index, *pointer))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{GFP_ATOMIC, GFP_KERNEL};

    #[repr(C)]
    struct TestXArray {
        lock: Spinlock,
        flags: u32,
        head: *mut XaEntries,
    }

    #[test]
    fn indexed_lookup_and_cyclic_allocation_match_xarray_contract() {
        let mut xa = TestXArray {
            // SAFETY: An all-zero byte pattern is a valid initial state for Spinlock.
            lock: unsafe { core::mem::zeroed() },
            flags: 0,
            head: core::ptr::null_mut(),
        };
        xa_init_flags(&mut xa, XA_FLAGS_ALLOC);
        let a = 1u32;
        let b = 2u32;
        let mut index = 0;
        let mut next = 0;
        assert_eq!(
            xa_alloc_cyclic_irq(
                &mut xa,
                &mut index,
                (&a as *const u32).cast_mut(),
                XA_LIMIT_32B,
                &mut next,
                GFP_KERNEL
            ),
            0
        );
        assert_eq!(index, 0);
        assert_eq!(xa_load::<_, u32>(&mut xa, 0), &a as *const u32 as *mut u32);
        assert_eq!(
            xa_alloc_cyclic_irq(
                &mut xa,
                &mut index,
                (&b as *const u32).cast_mut(),
                XA_LIMIT_32B,
                &mut next,
                GFP_KERNEL
            ),
            0
        );
        assert_eq!(index, 1);
        assert_eq!(
            xa_erase_irq::<_, u32>(&mut xa, 0),
            &a as *const u32 as *mut u32
        );
        xa_destroy(&mut xa);
    }

    #[test]
    fn irq_allocator_fails_closed_for_nonblocking_gfp() {
        let mut xa = TestXArray {
            // SAFETY: An all-zero byte pattern is a valid initial state for Spinlock.
            lock: unsafe { core::mem::zeroed() },
            flags: 0,
            head: core::ptr::null_mut(),
        };
        let mut index = 0;
        let mut next = 0;
        assert_eq!(
            xa_alloc_cyclic_irq(
                &mut xa,
                &mut index,
                core::ptr::null_mut::<u8>(),
                XA_LIMIT_32B,
                &mut next,
                GFP_ATOMIC
            ),
            -12
        );
    }
}

const _: [(); 0] = [(); offset_of!(XArrayHeader, xa_lock)];

#[macro_export]
macro_rules! xa_for_each {
    ($array:expr, $index:ident, $entry:ident, $body:block) => {{
        let __xa_entries = $crate::xarray::xa_snapshot($array);
        for (__xa_index, __xa_entry) in __xa_entries {
            let mut $index: u32 = __xa_index;
            let mut $entry: *mut _ = __xa_entry.cast();
            $body
        }
    }};
}

// ---------------------------------------------------------------------------
// C ABI XArray entry points.
//
// Indices are stored as `u32` in this backend. Larger Linux indices cannot be
// represented and fail with -EINVAL (store) or find nothing (lookups).
// Marks are never set by this backend, so only `XA_PRESENT` filters match.
// ---------------------------------------------------------------------------

/// Linux `XA_PRESENT`: matches any present entry.
const XA_PRESENT: u32 = 8;

/// Encode a negative errno as an error pointer, as `xa_err()` does.
#[inline]
fn xa_err(errno: i32) -> *mut c_void {
    (errno as isize) as *mut c_void
}

/// Run `f` with the XArray lock held, using the irq-safe form when the array
/// was created with `XA_FLAGS_LOCK_IRQ`.
///
/// # Safety
///
/// `xa` must point to a live, properly aligned, and initialized `XArray` structure.
unsafe fn with_xa_lock<R>(xa: *mut XArray, f: impl FnOnce() -> R) -> R {
    // SAFETY: Per function safety contract, `xa` points to a live, aligned `XArray`.
    let array = unsafe { &mut *xa };
    if array.xa_flags & XA_FLAGS_LOCK_IRQ != 0 {
        let mut flags = 0u64;
        xa_lock_irqsave(array, &mut flags);
        let result = f();
        xa_unlock_irqrestore(array, flags);
        result
    } else {
        xa_lock(array);
        let result = f();
        xa_unlock(array);
        result
    }
}

/// Linux `xa_store()`: store `entry` at `index` and return the previous entry.
/// A NULL entry erases the index.
///
/// # Safety
/// `xa` must point to a live initialized XArray and `entry`, when non-null,
/// must remain a valid opaque entry pointer for as long as it is stored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xa_store(
    xa: *mut XArray,
    index: c_ulong,
    entry: *mut c_void,
    _gfp: u32,
) -> *mut c_void {
    assert!(!xa.is_null());
    if index > u64::from(u32::MAX) {
        return xa_err(-crate::config::EINVAL);
    }
    // SAFETY: `xa` is checked non-null and valid per function contract; `with_xa_lock` synchronizes
    // access while calling `__xa_store` or `__xa_erase`.
    unsafe {
        with_xa_lock(xa, || {
            if entry.is_null() {
                __xa_erase::<XArray, c_void>(&mut *xa, index as u32)
            } else {
                __xa_store(&mut *xa, index as u32, entry, 0)
            }
        })
    }
}

/// Linux `xa_erase()`: remove `index` and return the previous entry.
///
/// # Safety
/// `xa` must point to a live initialized XArray.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xa_erase(xa: *mut XArray, index: c_ulong) -> *mut c_void {
    assert!(!xa.is_null());
    if index > u64::from(u32::MAX) {
        return core::ptr::null_mut();
    }
    // SAFETY: `xa` is non-null and valid per contract; `with_xa_lock` synchronizes access.
    unsafe { with_xa_lock(xa, || __xa_erase::<XArray, c_void>(&mut *xa, index as u32)) }
}

/// Shared search for `xa_find()` and `xa_find_after()`.
///
/// # Safety
///
/// `xa` must point to a live, initialized `XArray`, and `index` must point to
/// a readable and writable `c_ulong`.
unsafe fn xa_find_from(
    xa: *mut XArray,
    index: *mut c_ulong,
    max: c_ulong,
    filter: u32,
    start: u64,
) -> *mut c_void {
    if filter != XA_PRESENT || start > max || start > u64::from(u32::MAX) {
        return core::ptr::null_mut();
    }
    let last = max.min(u64::from(u32::MAX)) as u32;
    let first = start as u32;
    // SAFETY: `xa` and `index` are valid per safety contract; locked iteration over
    // entries map does not mutate keys, and writes match to `*index`.
    unsafe {
        with_xa_lock(xa, || {
            let head = entries(xa, false);
            if head.is_null() {
                return core::ptr::null_mut();
            }
            for (&key, &value) in (*head).entries.range(first..=last) {
                if !value.is_null() {
                    *index = c_ulong::from(key);
                    return value;
                }
            }
            core::ptr::null_mut()
        })
    }
}

/// Linux `xa_find()`: first present entry at or after `*index`, up to `max`.
///
/// # Safety
/// `xa` must point to a live initialized XArray and `index` to a readable and
/// writable `c_ulong`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xa_find(
    xa: *mut XArray,
    index: *mut c_ulong,
    max: c_ulong,
    filter: u32,
) -> *mut c_void {
    assert!(!xa.is_null() && !index.is_null());
    // SAFETY: `xa` and `index` are verified non-null and valid per function safety contract.
    // `*index` is dereferenced from the validated readable pointer.
    unsafe { xa_find_from(xa, index, max, filter, *index) }
}

/// Linux `xa_find_after()`: first present entry strictly after `*index`.
///
/// # Safety
/// `xa` must point to a live initialized XArray and `index` to a readable and
/// writable `c_ulong`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xa_find_after(
    xa: *mut XArray,
    index: *mut c_ulong,
    max: c_ulong,
    filter: u32,
) -> *mut c_void {
    assert!(!xa.is_null() && !index.is_null());
    // SAFETY: `index` is verified non-null and points to a readable `c_ulong` per caller contract.
    let Some(start) = (unsafe { *index }).checked_add(1) else {
        return core::ptr::null_mut();
    };
    // SAFETY: `xa` and `index` are verified non-null and valid per function safety contract.
    unsafe { xa_find_from(xa, index, max, filter, start) }
}

/// Linux `__xa_alloc()`: store `entry` at the lowest free id in `limit`. The
/// caller holds the XArray lock. Returns 0, -ENOMEM, or -EBUSY when `limit`
/// has no free id.
///
/// # Safety
/// `xa` must point to a live initialized XArray with its lock held by the
/// caller, `id` to writable storage, and `entry` to a valid opaque pointer
/// that remains live for as long as it is stored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __xa_alloc(
    xa: *mut XArray,
    id: *mut u32,
    entry: *mut c_void,
    limit: XaLimit,
    _gfp: u32,
) -> i32 {
    assert!(!xa.is_null() && !id.is_null());
    if entry.is_null() {
        // Reserving an id without an entry needs a zero-entry marker that this
        // backend does not store.
        return -crate::config::EINVAL;
    }
    if limit.min > limit.max {
        return -crate::config::EBUSY;
    }
    // SAFETY: `xa` is non-null and locked per caller contract, `id` points to writable storage,
    // and `entries(xa, true)` guarantees a valid `XaEntries` map is returned.
    unsafe {
        let head = entries(xa, true);
        let mut candidate = u64::from(limit.min);
        for (&key, _) in (*head).entries.range(limit.min..=limit.max) {
            if u64::from(key) != candidate {
                break;
            }
            candidate += 1;
        }
        if candidate > u64::from(limit.max) {
            return -crate::config::EBUSY;
        }
        (*head).entries.insert(candidate as u32, entry);
        *id = candidate as u32;
    }
    0
}
