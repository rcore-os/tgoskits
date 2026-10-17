// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux slab/allocation and reference-counting primitives used by the
//! source-ordered DRM bindings.  Allocation storage comes from the hosting kernel's
//! Rust global allocator; every returned pointer carries enough private
//! metadata for `kfree` to pass the exact original `Layout` back to it.
//!
//! This is deliberately not a Linux VM/reclaim emulator.  The global Rust
//! allocator has no GFP-aware or non-blocking entry point, so allocations that
//! may not sleep (including `GFP_ATOMIC` and `GFP_NOWAIT`) fail closed rather
//! than silently blocking in an IRQ/atomic context.  Sleepable allocations
//! (`GFP_KERNEL` and modifiers) use the kernel global allocator.

use alloc::{
    alloc::{Layout, alloc, alloc_zeroed, dealloc},
    collections::BTreeMap,
    vec::Vec,
};
use core::{
    ffi::{c_int, c_void},
    mem::{align_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, Ordering, fence},
};

use spin::Mutex;

use crate::{
    config::{GFP_ATOMIC, GFP_KERNEL},
    types::{AtomicT, Kref, RefcountT, Spinlock},
};

#[allow(non_camel_case_types)]
pub type kref = Kref;
#[allow(non_camel_case_types)]
pub type refcount_t = RefcountT;

// Linux 7.2.3 include/linux/gfp_types.h: ___GFP_DIRECT_RECLAIM_BIT follows
// ___GFP_UNUSED_BIT, hence bit 10 in the reference configuration used here.
const __GFP_DIRECT_RECLAIM: u32 = 1 << 10;
/// Linux 7.2.3 `___GFP_RETRY_MAYFAIL_BIT`.
pub const __GFP_RETRY_MAYFAIL: u32 = 1 << 14;
const __GFP_ZERO: u32 = 1 << 8;
const ZERO_SIZE_PTR: usize = 16;

#[repr(C)]
struct AllocationHeader {
    base: *mut u8,
    layout_size: usize,
    layout_align: usize,
    payload_size: usize,
}

/// Allocate raw bytes like Linux `kmalloc`.
///
/// `flags` must describe a sleepable allocation: this implementation rejects
/// requests lacking `__GFP_DIRECT_RECLAIM`, because the Rust global allocator
/// does not expose a GFP_ATOMIC/non-blocking allocator path.  Linux's
/// `__GFP_ZERO` modifier is honored in addition to the separate `kzalloc`
/// entry point.
pub fn kmalloc(size: usize, flags: u32) -> *mut c_void {
    allocate(size, align_of::<usize>(), flags, flags & __GFP_ZERO != 0).cast()
}

/// Linux `kmemdup_array()` with the kernel's checked element-count
/// multiplication and exact byte copy semantics.
///
/// Linux `kmemdup_array()` with the kernel's checked element-count
/// multiplication and exact byte copy semantics.
///
/// # Safety
///
/// - If `count * element_size > 0`, `source` must be non-null, properly aligned for reads, and point to at least `count * element_size` valid initialized bytes.
/// - `source` must not overlap with newly allocated memory.
pub unsafe fn kmemdup_array(
    source: *const c_void,
    count: usize,
    element_size: usize,
    flags: u32,
) -> *mut c_void {
    let Some(bytes) = count.checked_mul(element_size) else {
        return core::ptr::null_mut();
    };
    let copy = kmalloc(bytes, flags);
    if !copy.is_null() && bytes != 0 {
        // SAFETY:
        // - `bytes != 0` and `copy` is non-null from `kmalloc`, guaranteed to point to `bytes` writable bytes.
        // - `source` is valid for `bytes` reads per caller contract.
        // - `source` and `copy` do not overlap (`copy` is newly allocated).
        unsafe { core::ptr::copy_nonoverlapping(source.cast::<u8>(), copy.cast::<u8>(), bytes) };
    }
    copy
}

/// Allocate and zero raw bytes like Linux `kzalloc`.
pub fn kzalloc(size: usize, flags: u32) -> *mut c_void {
    allocate(size, align_of::<usize>(), flags, true).cast()
}

/// Linux `memset()` for translated fixed-layout records.
///
/// # Safety
///
/// - `destination` must be non-null, properly aligned, and valid for writes of `count` bytes.
/// - No concurrent reads or writes to the memory range may occur.
pub unsafe fn memset<T>(destination: *mut T, value: i32, count: usize) -> *mut T {
    if count != 0 {
        // SAFETY: `destination` is valid for writes of `count` bytes per caller contract, with exclusive access.
        unsafe { core::ptr::write_bytes(destination.cast::<u8>(), value as u8, count) };
    }
    destination
}

/// Free a non-null pointer returned by [`kmalloc`], [`kzalloc`], or one of
/// their object/array/flexible-array helpers.  Null and Linux's zero-size
/// sentinel are accepted, matching `kfree`.
///
/// # Safety
///
/// `object` must be null, the zero-size sentinel (`ZERO_SIZE_PTR`), or the exact pointer
/// returned by one of this module's allocation helpers, and must not have
/// already been freed.  As in Linux, freeing a foreign/interior/double-freed
/// pointer is invalid.
pub unsafe fn kfree<T>(object: *mut T) {
    let address = object as usize;
    if object.is_null() || address == ZERO_SIZE_PTR {
        return;
    }

    // SAFETY: `object` is verified non-null and not `ZERO_SIZE_PTR`. Per function contract, `object` was returned by `allocate()`, which placed an `AllocationHeader` immediately preceding the returned object address.
    let header = unsafe {
        (object.cast::<u8>().sub(size_of::<AllocationHeader>())).cast::<AllocationHeader>()
    };
    // SAFETY: `header` points to the valid initialized `AllocationHeader` written during allocation, aligned to `align_of::<AllocationHeader>()`.
    let metadata = unsafe { ptr::read(header) };
    let layout = Layout::from_size_align(metadata.layout_size, metadata.layout_align)
        .expect("kfree received corrupt allocation metadata");
    // SAFETY: `metadata.base` is the original allocation base pointer, `layout` matches the original allocation layout, and memory is freed exactly once.
    unsafe { dealloc(metadata.base, layout) };
}

/// Linux `kmalloc(size, flags)` ABI entry. Same allocator and GFP policy as
/// [`kmalloc`].
///
/// # Safety
///
/// Callers follow the Linux `kmalloc` contract; the returned pointer is freed
/// with [`kfree_c`] or `kvfree`.
#[unsafe(export_name = "kmalloc")]
pub unsafe extern "C" fn kmalloc_c(size: usize, flags: u32) -> *mut c_void {
    kmalloc(size, flags)
}

/// Linux `kfree(ptr)` ABI entry. NULL and ZERO_SIZE_PTR are no-ops.
///
/// # Safety
///
/// `object` must be NULL, `ZERO_SIZE_PTR`, or a live pointer returned by this allocator.
#[unsafe(export_name = "kfree")]
pub unsafe extern "C" fn kfree_c(object: *mut c_void) {
    // SAFETY: `object` satisfies the preconditions of `kfree` per caller contract.
    unsafe { kfree(object) };
}

/// Linux `memdup_user(src, len)`: allocate `len` bytes and copy them from the
/// current user address space. Failure returns `ERR_PTR(-ENOMEM)` or
/// `ERR_PTR(-EFAULT)`, never NULL.
///
/// # Safety
///
/// `src` is an untrusted user pointer; the copy is fault-checked by the
/// native usercopy path.
#[unsafe(export_name = "memdup_user")]
pub unsafe extern "C" fn memdup_user(src: *const c_void, len: usize) -> *mut c_void {
    let copy = kmalloc(len, GFP_KERNEL);
    if copy.is_null() {
        return crate::config::ERR_PTR(-crate::config::ENOMEM);
    }
    // SAFETY: `copy` points to `len` allocated bytes from `kmalloc`, and `crate::mm_native::copy_from_user` performs bounded copy with kernel fault handling.
    if len != 0 && unsafe { crate::mm_native::copy_from_user(copy, src, len) } != 0 {
        // SAFETY: `copy` was allocated by `kmalloc` above and is freed on failure.
        unsafe { kfree(copy) };
        return crate::config::ERR_PTR(-crate::config::EFAULT);
    }
    copy
}

/// Linux `drmm_kzalloc()` bookkeeping: each allocation is owned by the DRM
/// device it was requested for and is released by [`drmm_release_all`].
static DRMM_ALLOCATIONS: Mutex<BTreeMap<usize, Vec<usize>>> = Mutex::new(BTreeMap::new());

/// Linux `drmm_kzalloc(drm, size, flags)`. Returns NULL on failure. The
/// allocation lives until the DRM device's managed resources are released.
///
/// # Safety
///
/// `drm` must be a live DRM device identity that outlives this allocation until [`drmm_release_all`].
#[unsafe(export_name = "drmm_kzalloc")]
pub unsafe extern "C" fn drmm_kzalloc_c(drm: *mut c_void, size: usize, flags: u32) -> *mut c_void {
    if drm.is_null() {
        return ptr::null_mut();
    }
    let object = kzalloc(size, flags);
    if object.is_null() {
        return object;
    }
    DRMM_ALLOCATIONS
        .lock()
        .entry(drm as usize)
        .or_default()
        .push(object as usize);
    object
}

/// Release every `drmm_kzalloc()` allocation owned by `drm`, as Linux does
/// when the DRM device's managed-resource list is finalized.
///
/// # Safety
///
/// `drm` must be the device passed to earlier `drmm_kzalloc_c` calls and no
/// returned object may be used after this call.
pub unsafe fn drmm_release_all(drm: *mut c_void) {
    // drmm actions run before the managed allocations are freed, as in Linux's
    // drm_managed_release().
    // SAFETY: `drm` is a valid DRM device pointer per caller contract.
    unsafe { crate::kernel_core::drmm_run_actions(drm) };
    let owned = DRMM_ALLOCATIONS.lock().remove(&(drm as usize));
    for object in owned.into_iter().flatten() {
        // SAFETY: `object` was allocated via `kzalloc` in `drmm_kzalloc_c` and is freed exactly once here upon device finalization.
        unsafe { kfree(object as *mut c_void) };
    }
}

/// Registered `struct shrinker` objects. The hosting kernel has no reclaim pass that
/// calls `scan_objects`: the global allocator never reports pressure, so a
/// registered shrinker is retained and never invoked, as Linux does with no
/// memory pressure.
static SHRINKERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Linux `shrinker_alloc(flags, fmt, ...)`: a zeroed `struct shrinker`. The
/// name format is only used by Linux's debugfs/sysfs naming and is ignored.
///
/// # Safety
///
/// The returned object is released with [`shrinker_free`].
#[unsafe(export_name = "shrinker_alloc")]
pub unsafe extern "C" fn shrinker_alloc(
    _flags: u32,
    _fmt: *const core::ffi::c_char,
    mut _args: ...
) -> *mut crate::gem_memory::Shrinker {
    kzalloc(size_of::<crate::gem_memory::Shrinker>(), GFP_KERNEL).cast()
}

/// Linux `shrinker_register(shrinker)`.
///
/// # Safety
///
/// `shrinker` must come from [`shrinker_alloc`] and be initialized.
#[unsafe(export_name = "shrinker_register")]
pub unsafe extern "C" fn shrinker_register(shrinker: *mut crate::gem_memory::Shrinker) {
    let mut registered = SHRINKERS.lock();
    assert!(
        !registered.contains(&(shrinker as usize)),
        "shrinker registered twice"
    );
    registered.push(shrinker as usize);
}

/// Linux `shrinker_free(shrinker)`: unregisters if needed and frees the object.
///
/// # Safety
///
/// `shrinker` must come from [`shrinker_alloc`] and not be used afterwards.
#[unsafe(export_name = "shrinker_free")]
pub unsafe extern "C" fn shrinker_free(shrinker: *mut crate::gem_memory::Shrinker) {
    if shrinker.is_null() {
        return;
    }
    SHRINKERS.lock().retain(|&entry| entry != shrinker as usize);
    // SAFETY: `shrinker` was allocated by `kzalloc` in `shrinker_alloc`, is unregistered from `SHRINKERS`, and is freed exactly once here.
    unsafe { kfree(shrinker) };
}

/// Notifier chains that the hosting kernel never fires: no OOM killer and no lazy
/// vmap purge exist in this kernel. Registration is kept so a later event
/// source sees the same chain Linux would.
static OOM_NOTIFIERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static VMAP_PURGE_NOTIFIERS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn notifier_register(
    chain: &Mutex<Vec<usize>>,
    nb: *mut crate::gem_memory::NotifierBlock,
) -> c_int {
    if nb.is_null() {
        return -crate::config::EINVAL;
    }
    let mut chain = chain.lock();
    if !chain.contains(&(nb as usize)) {
        chain.push(nb as usize);
    }
    0
}

fn notifier_unregister(
    chain: &Mutex<Vec<usize>>,
    nb: *mut crate::gem_memory::NotifierBlock,
) -> c_int {
    let mut chain = chain.lock();
    let before = chain.len();
    chain.retain(|&entry| entry != nb as usize);
    if chain.len() == before {
        -crate::config::ENOENT
    } else {
        0
    }
}

/// Linux `register_oom_notifier(nb)`.
///
/// # Safety
///
/// `nb` must point to a live, properly aligned `NotifierBlock` that remains valid until unregistered.
#[unsafe(export_name = "register_oom_notifier")]
pub unsafe extern "C" fn register_oom_notifier(nb: *mut crate::gem_memory::NotifierBlock) -> c_int {
    notifier_register(&OOM_NOTIFIERS, nb)
}

/// Linux `unregister_oom_notifier(nb)`: `-ENOENT` if `nb` was not registered.
///
/// # Safety
///
/// `nb` must be a block previously passed to `register_oom_notifier`.
#[unsafe(export_name = "unregister_oom_notifier")]
pub unsafe extern "C" fn unregister_oom_notifier(
    nb: *mut crate::gem_memory::NotifierBlock,
) -> c_int {
    notifier_unregister(&OOM_NOTIFIERS, nb)
}

/// Linux `register_vmap_purge_notifier(nb)`.
///
/// # Safety
///
/// `nb` must point to a live, properly aligned `NotifierBlock` that remains valid until unregistered.
#[unsafe(export_name = "register_vmap_purge_notifier")]
pub unsafe extern "C" fn register_vmap_purge_notifier(
    nb: *mut crate::gem_memory::NotifierBlock,
) -> c_int {
    notifier_register(&VMAP_PURGE_NOTIFIERS, nb)
}

/// Linux `unregister_vmap_purge_notifier(nb)`.
///
/// # Safety
///
/// `nb` must be a block previously passed to `register_vmap_purge_notifier`.
#[unsafe(export_name = "unregister_vmap_purge_notifier")]
pub unsafe extern "C" fn unregister_vmap_purge_notifier(
    nb: *mut crate::gem_memory::NotifierBlock,
) -> c_int {
    notifier_unregister(&VMAP_PURGE_NOTIFIERS, nb)
}

/// Linux `kvmalloc_array()` for CPU-only buffers. The kernel allocator supplies
/// virtual addressability but not DMA-contiguous backing, matching Linux's
/// fallback allocation contract; overflowing dimensions fail without wrap.
///
/// # Safety
///
/// `flags` must describe an allocation policy supported by this allocator; the
/// returned pointer must be freed with this module's allocator functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kvmalloc_array(
    count: usize,
    element_size: usize,
    flags: u32,
) -> *mut c_void {
    let Some(size) = count.checked_mul(element_size) else {
        return ptr::null_mut();
    };
    kmalloc(size, flags)
}

/// Linux `kmalloc_array()` with checked multiplication and exact caller GFP
/// policy. Atomic allocations remain fail-closed in this global-allocator
/// implementation.
///
/// # Safety
///
/// `flags` must describe an allocation policy supported by this allocator; the
/// returned pointer must be freed with this module's allocator functions.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmalloc_array(
    count: usize,
    element_size: usize,
    flags: u32,
) -> *mut c_void {
    let Some(size) = count.checked_mul(element_size) else {
        return ptr::null_mut();
    };
    kmalloc(size, flags)
}

/// Linux `kvfree()` accepts either kmalloc or vmalloc backing. This owner uses
/// a single virtual allocator for both paths, so both share the same header.
///
/// # Safety
///
/// `object` must be null, the zero-size sentinel, or an unfreed pointer
/// returned by this module's allocation helpers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kvfree(object: *mut c_void) {
    // SAFETY: `object` satisfies the preconditions of `kfree` per caller contract.
    unsafe { kfree(object) };
}

/// Linux `krealloc()` with the old allocation's exact payload length retained
/// by the private allocator header. On allocation failure, the old allocation
/// remains live, as in Linux.
///
/// # Safety
///
/// `object` must be null, the zero-size sentinel, or an unfreed pointer
/// returned by this module's allocation helpers; aliases must not be used
/// after a successful reallocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn krealloc(object: *mut c_void, new_size: usize, flags: u32) -> *mut c_void {
    if object.is_null() {
        return kmalloc(new_size, flags);
    }
    if object as usize == ZERO_SIZE_PTR {
        return kmalloc(new_size, flags);
    }
    if new_size == 0 {
        // SAFETY: `object` was allocated by this allocator and is freed when reallocated to size 0.
        unsafe { kfree(object) };
        return ZERO_SIZE_PTR as *mut c_void;
    }
    // SAFETY: `object` is verified non-null and not `ZERO_SIZE_PTR`, so an `AllocationHeader` precedes it in memory.
    let header = unsafe {
        (object.cast::<u8>().sub(size_of::<AllocationHeader>())).cast::<AllocationHeader>()
    };
    // SAFETY: `header` points to the valid initialized `AllocationHeader` for `object`.
    let old_size = unsafe { (*header).payload_size };
    let replacement = kmalloc(new_size, flags);
    if replacement.is_null() {
        return ptr::null_mut();
    }
    // SAFETY:
    // - `object` is valid for reads up to `old_size`.
    // - `replacement` is non-null and valid for writes up to `new_size`.
    // - Copy length is `min(old_size, new_size)`.
    // - Source and destination do not overlap (`replacement` is a newly allocated block).
    // - `object` is freed via `kfree` and not used again.
    unsafe {
        ptr::copy_nonoverlapping(
            object.cast::<u8>(),
            replacement.cast::<u8>(),
            old_size.min(new_size),
        );
        kfree(object);
    }
    replacement
}

/// Allocate one uninitialized object with the requested GFP policy.
pub fn kmalloc_obj<T>(flags: u32) -> *mut T {
    allocate(
        size_of::<T>(),
        align_of::<T>(),
        flags,
        flags & __GFP_ZERO != 0,
    )
    .cast()
}

/// Allocate one zero-initialized object with `GFP_KERNEL`, as the common
/// source `kzalloc_obj` call form does.
pub fn kzalloc_obj<T>() -> *mut T {
    kzalloc_obj_flags::<T>(GFP_KERNEL)
}

/// Allocate one zero-initialized object with explicit GFP flags.
pub fn kzalloc_obj_flags<T>(flags: u32) -> *mut T {
    allocate(size_of::<T>(), align_of::<T>(), flags, true).cast()
}

/// Allocate an uninitialized array with `GFP_KERNEL`.
pub fn kmalloc_objs<T, N>(count: N) -> *mut T
where
    N: TryInto<usize>,
{
    kmalloc_objs_flags::<T, N>(count, GFP_KERNEL)
}

/// Allocate a variable-sized array with Linux `kvmalloc_array()` semantics
/// for CPU-only backing storage. This allocator provides virtual addressability
/// but does not promise physical contiguity, so it must not be used for
/// DMA-visible arrays.
pub fn kvmalloc_objs<T, N>(count: N) -> *mut T
where
    N: TryInto<usize>,
{
    kmalloc_objs_flags::<T, N>(count, GFP_KERNEL)
}

/// Allocate a zero-initialized array with `GFP_KERNEL`.
pub fn kzalloc_objs<T, N>(count: N) -> *mut T
where
    N: TryInto<usize>,
{
    kzalloc_objs_flags::<T, N>(count, GFP_KERNEL)
}

/// Allocate an uninitialized array with explicit GFP flags.
pub fn kmalloc_objs_flags<T, N>(count: N, flags: u32) -> *mut T
where
    N: TryInto<usize>,
{
    let Ok(count) = count.try_into() else {
        return ptr::null_mut();
    };
    let Some(size) = size_of::<T>().checked_mul(count) else {
        return ptr::null_mut();
    };
    allocate(size, align_of::<T>(), flags, flags & __GFP_ZERO != 0).cast()
}

/// Allocate a zero-initialized array with explicit GFP flags.
pub fn kzalloc_objs_flags<T, N>(count: N, flags: u32) -> *mut T
where
    N: TryInto<usize>,
{
    let Ok(count) = count.try_into() else {
        return ptr::null_mut();
    };
    let Some(size) = size_of::<T>().checked_mul(count) else {
        return ptr::null_mut();
    };
    allocate(size, align_of::<T>(), flags, true).cast()
}

/// Flexible-array field element sizing used by the `kzalloc_flex!` binding.
#[doc(hidden)]
pub trait FlexibleArray {
    const ELEMENT_SIZE: usize;
}

impl<T, const N: usize> FlexibleArray for [T; N] {
    const ELEMENT_SIZE: usize = size_of::<T>();
}

/// Allocate and zero a C-style structure followed by `count` trailing array
/// elements.  `member` is a type-inference closure for the flexible member;
/// `member_offset` is its source-layout offset.
#[doc(hidden)]
pub fn kzalloc_flex_impl<T, A, N, F>(count: N, member_offset: usize, member: F) -> *mut T
where
    A: FlexibleArray,
    N: TryInto<usize>,
    F: FnOnce(*mut T) -> *mut A,
{
    let Ok(count) = count.try_into() else {
        return ptr::null_mut();
    };
    // Constructing a dangling base is sufficient: the closure only forms a
    // raw field address and must not read the object.
    let base = ptr::NonNull::<T>::dangling().as_ptr();
    let _member = member(base);
    let Some(tail) = A::ELEMENT_SIZE.checked_mul(count) else {
        return ptr::null_mut();
    };
    let Some(size) = size_of::<T>().max(member_offset).checked_add(tail) else {
        return ptr::null_mut();
    };
    allocate(size, align_of::<T>(), GFP_KERNEL, true).cast()
}

/// Atomic read (`atomic_read`, relaxed ordering).
#[inline]
pub fn atomic_read(value: &AtomicT) -> i32 {
    atomic(value).load(Ordering::Relaxed)
}

/// Atomic set (`atomic_set`, relaxed ordering).
#[inline]
pub fn atomic_set(value: &mut AtomicT, new_value: i32) {
    atomic(value).store(new_value, Ordering::Relaxed);
}

/// Atomic increment (`atomic_inc`, relaxed ordering).
#[inline]
pub fn atomic_inc(value: &mut AtomicT) {
    atomic(value).fetch_add(1, Ordering::Relaxed);
}

/// `atomic_inc_not_zero()` with Linux's full ordering on successful RMW.
#[inline]
pub fn atomic_inc_not_zero(value: &mut AtomicT) -> bool {
    let counter = atomic(value);
    let mut old = counter.load(Ordering::Relaxed);
    while old != 0 {
        match counter.compare_exchange_weak(old, old + 1, Ordering::AcqRel, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(actual) => old = actual,
        }
    }
    false
}

/// Linux `atomic_dec_and_lock_irqsave()`: decrement with full atomic
/// ordering; when this observes the last reference, acquire the supplied raw
/// spinlock with local IRQ state saved before returning true.
///
/// # Safety
///
/// - `value` and `lock` must be live, properly aligned initialized objects.
/// - `flags` must point to writable `c_ulong` storage.
/// - The counter must be positive.
/// - If this returns true, the caller must release `lock` using the saved flags.
pub unsafe fn atomic_dec_and_lock_irqsave(
    value: *mut AtomicT,
    lock: *mut Spinlock,
    flags: *mut core::ffi::c_ulong,
) -> bool {
    // SAFETY: `value` is non-null, aligned, and valid for atomic operations per caller contract.
    let old = atomic(unsafe { &mut *value }).fetch_sub(1, Ordering::AcqRel);
    if old != 1 {
        return false;
    }
    let mut saved = 0;
    // SAFETY: `lock` is a live initialized spinlock per caller contract, and `saved` is a local stack variable.
    unsafe { crate::locks::spin_lock_irqsave_raw(lock, &mut saved) };
    // SAFETY: `flags` is valid for writes per caller contract.
    unsafe { *flags = saved };
    true
}

/// Atomic decrement (`atomic_dec`, relaxed ordering).
#[inline]
pub fn atomic_dec(value: &mut AtomicT) {
    atomic(value).fetch_sub(1, Ordering::Relaxed);
}

/// Linux SMP read barrier (`smp_rmb`).
#[inline]
pub fn smp_rmb() {
    fence(Ordering::Acquire);
}

/// Atomic addition (`atomic_add`, relaxed ordering).
#[inline]
pub fn atomic_add(addend: i32, value: &mut AtomicT) {
    atomic(value).fetch_add(addend, Ordering::Relaxed);
}

/// Atomic subtraction (`atomic_sub`, relaxed ordering).
#[inline]
pub fn atomic_sub(subtrahend: i32, value: &mut AtomicT) {
    atomic(value).fetch_sub(subtrahend, Ordering::Relaxed);
}

/// Atomic add-unless (`atomic_add_unless`, full ordering).
#[inline]
pub fn atomic_add_unless(value: &mut AtomicT, addend: i32, unless: i32) -> bool {
    let counter = atomic(value);
    let mut old = counter.load(Ordering::Relaxed);
    loop {
        if old == unless {
            return false;
        }
        match counter.compare_exchange_weak(
            old,
            old.wrapping_add(addend),
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return true,
            Err(observed) => old = observed,
        }
    }
}

/// Atomic decrement-and-test (`atomic_dec_and_test`, full ordering).
#[inline]
pub fn atomic_dec_and_test(value: &mut AtomicT) -> bool {
    atomic(value).fetch_sub(1, Ordering::SeqCst).wrapping_sub(1) == 0
}

/// Atomic subtract-and-test (`atomic_sub_and_test`, full ordering).
#[inline]
pub fn atomic_sub_and_test(subtrahend: i32, value: &mut AtomicT) -> bool {
    atomic(value)
        .fetch_sub(subtrahend, Ordering::SeqCst)
        .wrapping_sub(subtrahend)
        == 0
}

/// Atomic fetch-increment (`atomic_fetch_inc`, full ordering), returning the
/// value before incrementing.
#[inline]
pub fn atomic_fetch_inc(value: &mut AtomicT) -> i32 {
    atomic(value).fetch_add(1, Ordering::SeqCst)
}

/// Initialize a Linux `kref` to one reference.
///
/// # Safety
///
/// `value` must point to writable, properly aligned `Kref` storage with no
/// concurrent users or previous references.
#[inline]
pub unsafe fn kref_init(value: *mut Kref) {
    // SAFETY: `value` is guaranteed by caller to be non-null, properly aligned, and exclusively accessed for initialization.
    let reference = unsafe { &mut (*value).refcount };
    refcount_set(reference, 1);
}

/// Increment a live `kref` (`kref_get`).
///
/// As in Linux, the caller must already own a live reference: this operation
/// is not a lookup-safe way to resurrect an object whose count reached zero.
/// A violated precondition follows `refcount_inc()`'s fail-closed invariant
/// behavior by saturating the count and warning rather than wrapping or
/// manufacturing a reference from zero.
///
/// # Safety
///
/// `value` must point to a live `kref` whose storage remains valid throughout
/// the call, and the caller must own a reference that keeps its object alive.
#[inline]
pub unsafe fn kref_get(value: *mut Kref) {
    // SAFETY: `value` points to a live `Kref` per caller contract, and `&mut (*value).refcount` accesses the atomic counter.
    let got_reference = unsafe { refcount_inc_not_zero(&mut (*value).refcount) };
    if !got_reference {
        // `refcount_inc()` warns and saturates on a zero count (a likely UAF).
        // `inc_not_zero` intentionally leaves zero unchanged, so apply that
        // same invariant handling here rather than allowing a later wrap.
        // SAFETY: `value` points to a live `Kref` whose `refs` field is valid for atomic access.
        let counter = unsafe { atomic(&(*value).refcount.refs) };
        refcount_saturate(counter, "increment from zero");
    }
}

/// Drop one `kref` reference and call `release` only for the final reference.
///
/// Returns `1` exactly when this call performed the 1-to-0 transition and
/// invoked `release`; otherwise returns `0`, matching Linux's `int` result.
/// The callback is non-null by type, matching Linux's requirement that callers
/// provide the object's release function rather than `NULL` or a generic
/// `kfree`.
///
/// # Safety
///
/// `value` must point to a live `kref` with one reference owned by the caller.
/// `release` must be the correct destructor for the containing object and may
/// free it; it is called at most once for this decrement.
#[inline]
pub unsafe fn kref_put(value: *mut Kref, release: unsafe extern "C" fn(*mut Kref)) -> i32 {
    // SAFETY: `value` points to a live `Kref` per caller contract.
    if unsafe { refcount_dec_and_test(&mut (*value).refcount) } {
        // SAFETY: The reference count reached zero, so the final reference is dropped and `release` is invoked to destroy the object.
        unsafe { release(value) };
        1
    } else {
        0
    }
}

/// Read a reference count.  The generic view accepts the embedded `refcount_t`
/// used by dma-fence as well as the enclosing `kref` used by DRM objects.
#[inline]
pub fn kref_read<T: RefcountView + ?Sized>(value: &T) -> u32 {
    value.counter().load(Ordering::Relaxed) as u32
}

/// Increment a `kref` unless it has reached zero (`kref_get_unless_zero`).
#[inline]
pub fn kref_get_unless_zero(value: &mut Kref) -> bool {
    refcount_inc_not_zero(&mut value.refcount)
}

/// Set a Linux `refcount_t` using its relaxed initialization ordering.
#[inline]
pub fn refcount_set(value: &mut RefcountT, new_value: i32) {
    atomic(&value.refs).store(new_value, Ordering::Relaxed);
}

/// Read a Linux `refcount_t` (`refcount_read`, relaxed ordering).
#[inline]
pub fn refcount_read(value: &RefcountT) -> u32 {
    atomic(&value.refs).load(Ordering::Relaxed) as u32
}

/// Increment a `refcount_t` unless it is zero.  Invalid overflows saturate at
/// Linux's `REFCOUNT_SATURATED` value instead of wrapping back to zero.
pub fn refcount_inc_not_zero(value: &mut RefcountT) -> bool {
    let counter = atomic(&value.refs);
    let mut old = counter.load(Ordering::Relaxed);
    loop {
        if old == 0 {
            return false;
        }
        if old < 0 || old == i32::MAX {
            refcount_saturate(counter, "increment overflow");
            return true;
        }
        match counter.compare_exchange_weak(old, old + 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(observed) => old = observed,
        }
    }
}

/// Decrement a `refcount_t`, returning true only for the valid 1-to-0
/// transition (`refcount_dec_and_test`).
pub fn refcount_dec_and_test(value: &mut RefcountT) -> bool {
    let counter = atomic(&value.refs);
    let old = counter.fetch_sub(1, Ordering::Release);
    if old == 1 {
        fence(Ordering::Acquire);
        return true;
    }
    if old <= 0 {
        refcount_saturate(counter, "decrement underflow");
    }
    false
}

/// Decrement without testing (`refcount_dec`).  Linux deliberately leaks on a
/// decrement from one or below: callers that need to release the final object
/// must use `refcount_dec_and_test` and run its destructor themselves.
pub fn refcount_dec(value: &mut RefcountT) {
    let counter = atomic(&value.refs);
    let old = counter.fetch_sub(1, Ordering::Release);
    if old <= 1 {
        refcount_saturate(counter, "decrement would release without test");
    }
}

fn allocate(size: usize, alignment: usize, flags: u32, zero: bool) -> *mut u8 {
    if size == 0 {
        return ZERO_SIZE_PTR as *mut u8;
    }
    if !supports_sleepable_allocation(flags) {
        return ptr::null_mut();
    }

    let Ok(payload) = Layout::from_size_align(size, alignment.max(1)) else {
        return ptr::null_mut();
    };
    let Ok((layout, payload_offset)) = Layout::new::<AllocationHeader>().extend(payload) else {
        return ptr::null_mut();
    };
    let layout = layout.pad_to_align();
    // SAFETY: `layout` has non-zero size and valid alignment calculated from `Layout::extend`.
    let base = unsafe {
        if zero {
            alloc_zeroed(layout)
        } else {
            alloc(layout)
        }
    };
    if base.is_null() {
        return ptr::null_mut();
    }

    // `Layout::extend` aligns the payload after the header.  The header itself
    // is placed immediately before the returned address; because the payload
    // alignment is at least the header alignment, this address is aligned too.
    // SAFETY: `base` is non-null and valid for `layout.size()` bytes; `payload_offset <= layout.size()`.
    let object = unsafe { base.add(payload_offset) };
    // SAFETY: `payload_offset >= size_of::<AllocationHeader>()` per `Layout::extend`, so `object.sub(...)` is within the allocation and aligned to `AllocationHeader`.
    let header = unsafe { object.sub(size_of::<AllocationHeader>()) }.cast::<AllocationHeader>();
    // SAFETY: `header` is properly aligned and points to valid uninitialized header memory within `base`.
    unsafe {
        ptr::write(
            header,
            AllocationHeader {
                base,
                layout_size: layout.size(),
                layout_align: layout.align(),
                payload_size: size,
            },
        );
    }
    object
}

fn supports_sleepable_allocation(flags: u32) -> bool {
    // GFP_ATOMIC contains __GFP_HIGH and __GFP_KSWAPD_RECLAIM, but not
    // __GFP_DIRECT_RECLAIM.  Testing the source direct-reclaim bit also rejects
    // GFP_NOWAIT and other non-sleepable combinations.  The `GFP_KERNEL` check
    // documents the only base policy enabled by this compatibility layer.
    let _known_sleepable_base = GFP_KERNEL;
    flags & __GFP_DIRECT_RECLAIM != 0 && flags & GFP_ATOMIC != GFP_ATOMIC
}

fn atomic(value: &AtomicT) -> &AtomicI32 {
    // AtomicT is the source-layout binding for Linux atomic_t: a repr(C) i32
    // counter at the same 4-byte alignment as AtomicI32.  All access to this
    // counter in the LinuxKPI layer is performed through this atomic view.
    let counter = ptr::addr_of!(value.counter).cast_mut();
    // SAFETY: AtomicT and AtomicI32 have matching size/alignment here, and the
    // referenced counter lives for at least the duration of the borrow.
    unsafe { AtomicI32::from_ptr(counter) }
}

/// Read-only view of the atomic counter embedded in either `refcount_t` or
/// `kref` layout bindings.
pub trait RefcountView {
    #[doc(hidden)]
    fn counter(&self) -> &AtomicI32;
}

impl RefcountView for RefcountT {
    fn counter(&self) -> &AtomicI32 {
        atomic(&self.refs)
    }
}

impl RefcountView for Kref {
    fn counter(&self) -> &AtomicI32 {
        atomic(&self.refcount.refs)
    }
}

fn refcount_saturate(counter: &AtomicI32, reason: &'static str) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    counter.store(i32::MIN / 2, Ordering::Relaxed);
    if !WARNED.swap(true, Ordering::Relaxed) {
        log::warn!("LinuxKPI refcount saturated: {}", reason);
    }
}

/// Allocate one object with `GFP_KERNEL`, preserving source C helper spelling.
#[macro_export]
macro_rules! kzalloc_obj {
    ($type:ty $(,)?) => {{ $crate::memory::kzalloc_obj::<$type>() }};
    ($type:ty, $flags:expr $(,)?) => {{ $crate::memory::kzalloc_obj_flags::<$type>($flags) }};
}

/// Allocate one uninitialized object with explicit GFP flags.
#[macro_export]
macro_rules! kmalloc_obj {
    ($type:ty $(,)?) => {{ $crate::memory::kmalloc_obj::<$type>($crate::config::GFP_KERNEL) }};
    ($type:ty, $flags:expr $(,)?) => {{ $crate::memory::kmalloc_obj::<$type>($flags) }};
}

/// Allocate an object array with `GFP_KERNEL` or explicit flags.
#[macro_export]
macro_rules! kmalloc_objs {
    ($type:ty, $count:expr $(,)?) => {{ $crate::memory::kmalloc_objs::<$type, _>($count) }};
    ($type:ty, $count:expr, $flags:expr $(,)?) => {{ $crate::memory::kmalloc_objs_flags::<$type, _>($count, $flags) }};
}

/// Allocate and zero an object array with `GFP_KERNEL` or explicit flags.
#[macro_export]
macro_rules! kzalloc_objs {
    ($type:ty, $count:expr $(,)?) => {{ $crate::memory::kzalloc_objs::<$type, _>($count) }};
    ($type:ty, $count:expr, $flags:expr $(,)?) => {{ $crate::memory::kzalloc_objs_flags::<$type, _>($count, $flags) }};
}

/// Allocate a zeroed C-style structure with trailing flexible-array members.
#[macro_export]
macro_rules! kzalloc_flex {
    ($type:ty, $member:ident, $count:expr $(,)?) => {{
        $crate::memory::kzalloc_flex_impl::<$type, _, _, _>(
            $count,
            core::mem::offset_of!($type, $member),
            // SAFETY: `addr_of_mut!` computes the field offset without creating a reference to `base`.
            |base: *mut $type| unsafe { core::ptr::addr_of_mut!((*base).$member) },
        )
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C, align(64))]
    struct CacheLine([u8; 73]);

    #[repr(C)]
    struct Flex {
        count: u32,
        values: [u64; 0],
    }

    #[test]
    fn allocation_tracks_layout_and_zeroing() {
        // SAFETY: Test validates allocation, access, and freeing of a typed object and raw byte slice via `kfree`.
        unsafe {
            let object = kzalloc_obj_flags::<CacheLine>(GFP_KERNEL);
            assert!(!object.is_null());
            assert_eq!(object as usize % align_of::<CacheLine>(), 0);
            assert!((*object).0.iter().all(|byte| *byte == 0));
            (*object).0[0] = 7;
            kfree(object);

            let bytes = kmalloc(17, GFP_KERNEL).cast::<u8>();
            assert!(!bytes.is_null());
            kfree(bytes);
        }
    }

    #[test]
    fn unsupported_atomic_allocation_fails_closed() {
        assert!(kmalloc(8, GFP_ATOMIC).is_null());
    }

    #[test]
    fn flexible_array_size_is_checked_and_zeroed() {
        // SAFETY: Test allocates a structure with flexible array members and frees it via `kfree`.
        unsafe {
            let count = 3usize;
            let value = kzalloc_flex_impl::<Flex, _, _, _>(
                count,
                core::mem::offset_of!(Flex, values),
                |base| ptr::addr_of_mut!((*base).values),
            );
            assert!(!value.is_null());
            let tail = ptr::addr_of_mut!((*value).values).cast::<u64>();
            assert_eq!(*tail.add(2), 0);
            kfree(value);
        }
        let too_many = kzalloc_objs::<u64, _>(u128::MAX);
        assert!(too_many.is_null());
    }

    #[test]
    fn atomic_and_refcount_ordered_transitions() {
        let mut atomic = AtomicT { counter: 0 };
        atomic_set(&mut atomic, 2);
        assert_eq!(atomic_read(&atomic), 2);
        assert!(atomic_add_unless(&mut atomic, 1, 0));
        assert_eq!(atomic_fetch_inc(&mut atomic), 3);
        assert!(!atomic_dec_and_test(&mut atomic));
        assert!(atomic_sub_and_test(3, &mut atomic));

        let mut reference = RefcountT {
            refs: AtomicT { counter: 1 },
        };
        assert!(refcount_inc_not_zero(&mut reference));
        assert_eq!(refcount_read(&reference), 2);
        assert!(!refcount_dec_and_test(&mut reference));
        assert!(refcount_dec_and_test(&mut reference));
        assert!(!refcount_inc_not_zero(&mut reference));
    }
}

#[cfg(test)]
mod kernel_service_tests {
    use super::*;
    use crate::{config::ENOENT, gem_memory::NotifierBlock};

    #[test]
    fn drmm_allocations_are_zeroed_and_released_per_device() {
        // SAFETY: Uses a dummy device pointer address to exercise drmm allocation tracking and release.
        unsafe {
            let drm = 0x5000_0000usize as *mut c_void;
            let first = drmm_kzalloc_c(drm, 64, GFP_KERNEL).cast::<u8>();
            assert!(!first.is_null());
            assert!((0..64).all(|offset| *first.add(offset) == 0));
            assert!(drmm_kzalloc_c(ptr::null_mut(), 8, GFP_KERNEL).is_null());
            drmm_release_all(drm);
            assert!(DRMM_ALLOCATIONS.lock().get(&(drm as usize)).is_none());
        }
    }

    #[test]
    fn oom_and_vmap_purge_chains_track_registration() {
        // SAFETY: Uses dummy non-null pointers as opaque keys for notifier registration tests.
        unsafe {
            let oom = 0x5100_0000usize as *mut NotifierBlock;
            assert_eq!(register_oom_notifier(oom), 0);
            assert_eq!(unregister_oom_notifier(oom), 0);
            assert_eq!(unregister_oom_notifier(oom), -ENOENT);

            let purge = 0x5200_0000usize as *mut NotifierBlock;
            assert_eq!(register_vmap_purge_notifier(purge), 0);
            assert_eq!(unregister_vmap_purge_notifier(purge), 0);
            assert_eq!(unregister_vmap_purge_notifier(purge), -ENOENT);
        }
    }

    #[test]
    fn shrinker_objects_round_trip_through_register_and_free() {
        // SAFETY: Exercises allocation, registration, and freeing of a Shrinker object with a static CStr format.
        unsafe {
            let shrinker = shrinker_alloc(0, c"test".as_ptr());
            assert!(!shrinker.is_null());
            shrinker_register(shrinker);
            shrinker_free(shrinker);
            assert!(SHRINKERS.lock().is_empty());
        }
    }
}
