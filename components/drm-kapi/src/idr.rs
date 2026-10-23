// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! Linux IDA surface backed by the LinuxKPI XArray indexed storage.

use alloc::{boxed::Box, collections::BTreeMap};
use core::ffi::{c_int, c_ulong, c_void};

use crate::{
    config::{GFP_ATOMIC, GFP_KERNEL},
    types::RadixTreeRoot,
    xarray::{self, XArray},
};

// The source DRM `idr` is accessed under RCU while handles are added
// and removed by other threads. This LinuxKPI implementation stores stable
// slots in a BTreeMap rather than a lockless radix tree, so serialize slot
// lookup/mutation and let the caller acquire the GEM reference before leaving
// its surrounding RCU read-side section.
static RADIX_SLOTS_LOCK: spin::Mutex<()> = spin::Mutex::new(());

/// `struct idr` (include/linux/idr.h): an xarray plus the allocation base and
/// next-free cursor. Oracle DWARF size 24, align 8.
#[repr(C)]
pub struct Idr {
    pub idr_rt: RadixTreeRoot,
    pub idr_base: u32,
    pub idr_next: u32,
}
const _: [(); 24] = [(); core::mem::size_of::<Idr>()];
const _: [(); 16] = [(); core::mem::offset_of!(Idr, idr_base)];

/// Linux `struct ida` embeds one `struct xarray` as its complete ABI.
#[repr(C)]
pub struct Ida {
    pub xa: XArray,
}

const _: [(); 16] = [(); core::mem::size_of::<Ida>()];
const _: [(); 0] = [(); core::mem::offset_of!(Ida, xa)];

pub fn ida_init(ida: &mut Ida) {
    xarray::xa_init_flags(&mut ida.xa, 0);
}

pub fn ida_destroy(ida: &mut Ida) {
    xarray::xa_destroy(&mut ida.xa);
}

/// Allocate the lowest unused ID in the inclusive range.
///
/// `IDA` stores an allocated bitmap entry per ID in the XArray. This binding
/// uses a non-null sentinel entry; the ID uniqueness and locking contract are
/// preserved, and the allocator's GFP_ATOMIC limitation is inherited from
/// the XArray backend.
pub fn ida_alloc_range(ida: &mut Ida, min: u32, max: u32, gfp: u32) -> i32 {
    if min > max {
        return -22;
    }
    if gfp & GFP_ATOMIC != 0 && gfp & GFP_KERNEL == 0 {
        return -12;
    }
    let mut flags = 0u64;
    xarray::xa_lock_irqsave(&mut ida.xa, &mut flags);
    let result = (min..=max).find(|id| xarray::xa_load::<_, u8>(&mut ida.xa, *id).is_null());
    let status = if let Some(id) = result {
        let marker = core::ptr::NonNull::<u8>::dangling().as_ptr();
        xarray::__xa_store(&mut ida.xa, id, marker, 0);
        id as i32
    } else {
        -28
    };
    xarray::xa_unlock_irqrestore(&mut ida.xa, flags);
    status
}

pub fn ida_free(ida: &mut Ida, id: u32) {
    let _ = xarray::xa_erase_irq::<_, u8>(&mut ida.xa, id);
}

// ---------------------------------------------------------------------------
// Radix tree and IDR.
//
// `struct radix_tree_root::rnode` points at a heap-allocated ordered map from
// index to a boxed slot. Each slot is a stable `*mut c_void` cell, so the
// slot pointers returned by the iterator stay valid while the entry is kept.
// Every chunk the iterator returns covers one slot, and no tags are kept.
// ---------------------------------------------------------------------------

/// Storage behind `radix_tree_root::rnode`.
struct RadixSlots {
    slots: BTreeMap<u64, Box<*mut c_void>>,
}

/// ABI mirror of `struct radix_tree_iter` from include/linux/radix-tree.h.
///
/// The fields are private because Linux callers should access them through the
/// radix-tree iterator helpers; the public opaque type lets Rust FFI callers
/// pass storage with the required C layout to those helpers.
#[repr(C)]
pub struct RadixIter {
    index: c_ulong,
    next_index: c_ulong,
    tags: c_ulong,
    node: *mut c_void,
}

const RADIX_TREE_ITER_TAGGED: u32 = 0x10;
const RADIX_TREE_ITER_CONTIG: u32 = 0x20;
const EEXIST_ERRNO: c_int = crate::config::EEXIST;
const EINVAL_ERRNO: c_int = crate::config::EINVAL;

/// The slot storage of `root`, created on first use.
///
/// # Safety
/// `root` must be aligned and point to a live, initialized `RadixTreeRoot` for
/// the returned lifetime; its `rnode` must be null or point to a `RadixSlots`
/// allocation created by this module. The caller must hold `RADIX_SLOTS_LOCK`
/// and ensure that no other thread accesses or destroys the root outside that
/// lock.
unsafe fn radix_slots_mut<'a>(root: *mut RadixTreeRoot) -> &'a mut RadixSlots {
    // SAFETY: `root` is non-null and valid per function safety contract, and `RADIX_SLOTS_LOCK`
    // is held. `rnode` is allocated via Box::into_raw or already points to a valid Box-managed `RadixSlots`.
    unsafe {
        if (*root).rnode.is_null() {
            let storage = Box::new(RadixSlots {
                slots: BTreeMap::new(),
            });
            (*root).rnode = Box::into_raw(storage).cast::<c_void>();
        }
        &mut *(*root).rnode.cast::<RadixSlots>()
    }
}

/// The slot storage of `root`, if any entry was ever stored.
///
/// # Safety
/// `root` must be aligned and point to a live, initialized `RadixTreeRoot` for
/// the returned lifetime; its `rnode` must be null or point to a `RadixSlots`
/// allocation created by this module. The caller must hold `RADIX_SLOTS_LOCK`
/// so `rnode` cannot be replaced or freed while the returned reference is
/// used.
unsafe fn radix_slots_ref<'a>(root: *const RadixTreeRoot) -> Option<&'a RadixSlots> {
    // SAFETY: `root` is valid per safety contract and `RADIX_SLOTS_LOCK` is held, ensuring
    // that if non-null, `rnode` points to an initialized `RadixSlots` that will not be concurrently freed.
    unsafe {
        let rnode = (*root).rnode;
        if rnode.is_null() {
            None
        } else {
            Some(&*rnode.cast::<RadixSlots>())
        }
    }
}

/// Free the slot storage of `root` once it holds no entries, so an emptied
/// tree does not keep its map allocated.
///
/// # Safety
/// `root` must be aligned and point to a live, writable `RadixTreeRoot`; its
/// `rnode` must be null or point to a live `RadixSlots` allocation created by
/// this module. The caller must hold `RADIX_SLOTS_LOCK` and exclude concurrent
/// access to the root and its slot storage.
unsafe fn radix_release_if_empty(root: *mut RadixTreeRoot) {
    // SAFETY: `root` is valid and locked per contract. If `rnode` is non-null and empty,
    // it was allocated via Box::into_raw in `radix_slots_mut`, so reconstructing the Box frees it safely.
    unsafe {
        let rnode = (*root).rnode;
        if !rnode.is_null() && (*rnode.cast::<RadixSlots>()).slots.is_empty() {
            drop(Box::from_raw(rnode.cast::<RadixSlots>()));
            (*root).rnode = core::ptr::null_mut();
        }
    }
}

/// Linux `radix_tree_destroy()`: free the tree's internal storage. The entries
/// are the caller's and are not released.
///
/// # Safety
/// `root` must be non-null, aligned, and point to a live, zero-initialized or
/// module-initialized `RadixTreeRoot` whose `rnode` is null or module-owned.
/// No operation may access the root or its slots concurrently, and no later
/// reader may use a slot pointer from this tree. Entry pointers are not freed;
/// their owner must keep them alive for any outstanding users and release
/// them separately.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn radix_tree_destroy(root: *mut RadixTreeRoot) {
    assert!(!root.is_null());
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `root` is checked non-null and locked. `rnode`, if non-null, was allocated via
    // `Box::into_raw` by `radix_slots_mut`, so reconstructing and dropping the Box releases the map safely.
    unsafe {
        let rnode = (*root).rnode;
        if !rnode.is_null() {
            drop(Box::from_raw(rnode.cast::<RadixSlots>()));
            (*root).rnode = core::ptr::null_mut();
        }
    }
}

/// Linux `radix_tree_insert()`: 0 on success, -EEXIST when `index` is taken.
///
/// # Safety
/// `root` must be non-null, aligned, and point to a live, initialized
/// `RadixTreeRoot` whose `rnode` is null or module-owned and that is not being
/// destroyed or reinitialized concurrently. `item` is stored as an opaque
/// pointer and is never owned or freed here; if readers dereference it, its
/// owner must keep it valid and aligned for the pointee type for their entire
/// use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn radix_tree_insert(
    root: *mut RadixTreeRoot,
    index: u64,
    item: *mut c_void,
) -> c_int {
    assert!(!root.is_null());
    if item.is_null() {
        return -EINVAL_ERRNO;
    }
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `root` is verified non-null and live per caller contract; `_guard` ensures exclusive lock.
    let slots = unsafe { radix_slots_mut(root) };
    if slots.slots.get(&index).is_some_and(|slot| !slot.is_null()) {
        return -EEXIST_ERRNO;
    }
    slots.slots.insert(index, Box::new(item));
    0
}

/// Linux `radix_tree_lookup()`.
///
/// # Safety
/// `root` must be non-null, aligned, and point to a live tree whose `rnode` is
/// null or module-owned.
/// The returned entry is an unowned raw pointer and is not pinned after the
/// internal lock is released; callers that dereference it must synchronize
/// against deletion/destruction or acquire their own lifetime reference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn radix_tree_lookup(root: *mut RadixTreeRoot, index: u64) -> *mut c_void {
    assert!(!root.is_null());
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `root` is non-null and live per caller contract; `_guard` guarantees synchronized access.
    match unsafe { radix_slots_ref(root) } {
        Some(slots) => slots
            .slots
            .get(&index)
            .map_or(core::ptr::null_mut(), |slot| **slot),
        None => core::ptr::null_mut(),
    }
}

/// Linux `radix_tree_delete()`: remove `index` and return the old entry.
///
/// # Safety
/// `root` must be non-null, aligned, and point to a live tree whose `rnode` is
/// null or module-owned and that is not being destroyed or reinitialized
/// concurrently. The returned pointer is not freed or reference-counted by
/// this function; the caller must preserve its owner and coordinate with any
/// readers that obtained it earlier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn radix_tree_delete(root: *mut RadixTreeRoot, index: u64) -> *mut c_void {
    assert!(!root.is_null());
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `root` is verified non-null and locked per caller contract.
    match unsafe { radix_slots_ref(root) } {
        Some(_) => {
            // SAFETY: `root` is valid and locked; returns exclusive reference to the slots map.
            let slots = unsafe { radix_slots_mut(root) };
            let removed = slots
                .slots
                .remove(&index)
                .map_or(core::ptr::null_mut(), |slot| *slot);
            // SAFETY: `root` is valid and locked; cleans up backing storage if empty.
            unsafe { radix_release_if_empty(root) };
            removed
        }
        None => core::ptr::null_mut(),
    }
}

/// Linux `radix_tree_next_chunk()`: the next present slot at or after
/// `iter->next_index`, or NULL at the end. `RADIX_TREE_ITER_CONTIG` stops at
/// the first hole, and tagged iteration finds nothing since no tags are kept.
///
/// # Safety
/// `root` must be non-null, aligned, and point to a live tree whose `rnode` is
/// null or module-owned.
/// `iter` must be non-null, aligned, and point to a live writable `RadixIter`
/// initialized according to the Linux radix-tree iterator contract. The
/// returned slot pointer is borrowed from the tree and remains valid only
/// while that slot is not removed and the root is not destroyed; callers must
/// externally synchronize mutations before retaining or dereferencing it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn radix_tree_next_chunk(
    root: *const RadixTreeRoot,
    iter: *mut RadixIter,
    flags: u32,
) -> *mut *mut c_void {
    assert!(!root.is_null() && !iter.is_null());
    let _guard = RADIX_SLOTS_LOCK.lock();
    if flags & RADIX_TREE_ITER_TAGGED != 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `iter` is non-null and valid per caller safety contract.
    let start = unsafe { (*iter).next_index };
    // A zero next_index after the first chunk marks the end of iteration.
    // SAFETY: `iter` is non-null and valid per caller safety contract.
    if start == 0 && unsafe { (*iter).index } != 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `root` is valid and locked per caller contract.
    let Some(slots) = (unsafe { radix_slots_ref(root) }) else {
        return core::ptr::null_mut();
    };
    let Some((&key, slot)) = slots.slots.range(start..).find(|(_, slot)| !slot.is_null()) else {
        return core::ptr::null_mut();
    };
    let slot_ptr: *mut *mut c_void = core::ptr::addr_of!(**slot).cast_mut();
    // SAFETY: `iter` is non-null, aligned, and exclusively writable under `_guard`.
    unsafe {
        if flags & RADIX_TREE_ITER_CONTIG != 0 && (*iter).index != 0 && key != start {
            (*iter).next_index = 0;
            return core::ptr::null_mut();
        }
        (*iter).index = c_ulong::try_from(key).unwrap_or(c_ulong::MAX);
        (*iter).next_index = (*iter).index.wrapping_add(1);
        (*iter).tags = 1;
        (*iter).node = core::ptr::null_mut();
    }
    slot_ptr
}

/// Linux `radix_tree_next_slot()`. Every chunk covers exactly one slot, so
/// the chunk is exhausted on return and the caller asks for the next chunk.
///
/// # Safety
/// This compatibility stub ignores all arguments and always returns null, so
/// it has no pointer-validity or ownership preconditions. Callers must not
/// rely on it to advance an iterator or preserve a slot pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn radix_tree_next_slot(
    _slot: *mut *mut c_void,
    _iter: *mut RadixIter,
    _flags: u32,
) -> *mut *mut c_void {
    core::ptr::null_mut()
}

/// Linux `radix_tree_iter_delete()`: remove the entry at `iter->index` and
/// clear `*slot`.
///
/// # Safety
/// `root` must be non-null, aligned, and point to a live tree whose `rnode` is
/// null or module-owned;
/// `iter` must be non-null, aligned, and point to a live readable `RadixIter`
/// whose `index` identifies the entry to remove. `slot` may be null; otherwise
/// it must point to a writable slot cell associated with that entry (normally
/// the cell returned by `radix_tree_next_chunk`). The caller must prevent
/// concurrent mutation of this entry between iteration and deletion, exclude
/// concurrent destruction, and preserve any removed entry for existing users;
/// this function only removes the tree slot and does not free the entry.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn radix_tree_iter_delete(
    root: *mut RadixTreeRoot,
    iter: *mut RadixIter,
    slot: *mut *mut c_void,
) {
    assert!(!root.is_null() && !iter.is_null());
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `root` is valid and locked per caller contract.
    if unsafe { radix_slots_ref(root) }.is_none() {
        return;
    }
    // SAFETY: `iter` is non-null and valid per caller contract.
    let index = unsafe { (*iter).index };
    // SAFETY: `root` is valid and locked; retrieves exclusive access to the tree slots.
    let slots = unsafe { radix_slots_mut(root) };
    if let Some(mut storage) = slots.slots.remove(&index) {
        *storage = core::ptr::null_mut();
        if !slot.is_null() {
            // SAFETY: `slot` is verified non-null and points to a writable slot per contract.
            unsafe { *slot = core::ptr::null_mut() };
        }
    }
    // SAFETY: `root` is valid and locked; frees empty backing storage.
    unsafe { radix_release_if_empty(root) };
}

pub const IDR_RESERVED_ENTRY: *mut c_void = usize::MAX as *mut c_void;

/// Linux `idr_init_base()`.
///
/// # Safety
/// `idr` may be null (in which case this function does nothing); otherwise it
/// must be aligned and point to a live, exclusively writable `Idr` allocation
/// that is uninitialized, zeroed, or already destroyed. Do not reinitialize an
/// IDR containing entries, since this overwrites its root pointer and leaks the
/// slot storage. No other thread may access the IDR during initialization.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_init_base(idr: *mut Idr, base: c_int) {
    if !idr.is_null() {
        // SAFETY: `idr` is non-null, aligned, and exclusively writable per caller contract.
        unsafe {
            core::ptr::write_bytes(idr, 0, 1);
            (*idr).idr_base = base as u32;
            (*idr).idr_next = base as u32;
        }
    }
}

/// Linux `idr_init()`.
///
/// # Safety
/// `idr` may be null (a no-op); otherwise it must satisfy the same exclusive,
/// empty-storage preconditions as [`idr_init_base`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_init(idr: *mut Idr) {
    // SAFETY: `idr` satisfies caller preconditions forwarded to `idr_init_base`.
    unsafe { idr_init_base(idr, 0) };
}

/// Linux `idr_destroy()`.
///
/// # Safety
/// `idr` may be null (a no-op); otherwise it must point to a live IDR
/// initialized by this module, with no concurrent access or outstanding slot
/// iterators. Entry values are caller-owned and are not released here; the
/// caller must ensure no reader still uses them and release them separately.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_destroy(idr: *mut Idr) {
    if !idr.is_null() {
        // SAFETY: `idr` is non-null and valid; delegates to `radix_tree_destroy` on the embedded root.
        unsafe { radix_tree_destroy(core::ptr::addr_of_mut!((*idr).idr_rt)) };
    }
}

/// Linux `idr_is_empty()`.
///
/// # Safety
/// `idr` may be null; otherwise it must be aligned and point to a live,
/// initialized IDR for the duration of the call. The global slot lock
/// serializes IDR operations, but the caller must not free or reinitialize the
/// IDR concurrently.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_is_empty(idr: *const Idr) -> bool {
    if idr.is_null() {
        return true;
    }
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `idr` is non-null and live per caller contract; queries the embedded root.
    match unsafe { radix_slots_ref(core::ptr::addr_of!((*idr).idr_rt)) } {
        Some(slots) => slots.slots.is_empty(),
        None => true,
    }
}

/// Linux `idr_alloc()`: allocates an ID in `[start, end)`. `end == 0` means INT_MAX.
///
/// # Safety
/// `idr` may be null (returning `-EINVAL`); otherwise it must point to a live,
/// initialized IDR that is not being destroyed or reinitialized concurrently.
/// `ptr` is stored but never dereferenced or freed here: null requests a
/// reserved ID, while a non-null entry must remain valid for any caller that
/// later dereferences it and must not equal [`IDR_RESERVED_ENTRY`]. The global
/// lock protects only slot storage; callers must use their owner lock/refcount
/// protocol to protect the IDR and entry lifetimes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_alloc(idr: *mut Idr, ptr: *mut c_void, start: i32, end: i32) -> i32 {
    if idr.is_null() {
        return -crate::config::EINVAL;
    }
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `idr` is verified non-null and valid per caller contract.
    let base = unsafe { (*idr).idr_base };
    let max = if end <= 0 { i32::MAX } else { end - 1 };
    let start_id = start.max(base as i32);
    if start_id > max {
        return -crate::config::ENOSPC;
    }
    // SAFETY: `idr` is valid and locked; accesses mutable slots of the embedded root.
    let slots = unsafe { radix_slots_mut(core::ptr::addr_of_mut!((*idr).idr_rt)) };
    let key_start = (start_id as u64).saturating_sub(base as u64);
    let key_max = (max as u64).saturating_sub(base as u64);
    let mut candidate_key = key_start;
    for (&k, _) in slots.slots.range(key_start..=key_max) {
        if k == candidate_key {
            candidate_key = candidate_key.wrapping_add(1);
        } else if k > candidate_key {
            break;
        }
    }
    if candidate_key > key_max {
        return -crate::config::ENOSPC;
    }
    let entry = if ptr.is_null() {
        IDR_RESERVED_ENTRY
    } else {
        ptr
    };
    slots.slots.insert(candidate_key, Box::new(entry));
    (candidate_key + base as u64) as i32
}

/// Linux `idr_replace()`: replaces the entry at `id` with `ptr`.
///
/// # Safety
/// `idr` may be null (returning null); otherwise it must point to a live,
/// initialized IDR that is not being destroyed or reinitialized concurrently.
/// `ptr` is stored as an opaque caller-owned value (null reserves the slot),
/// must not equal [`IDR_RESERVED_ENTRY`], and must remain valid for readers
/// that may dereference it. The returned old pointer is not freed or pinned.
/// The owning subsystem must serialize logical updates and protect entry
/// lifetimes independently of the internal storage lock.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_replace(idr: *mut Idr, ptr: *mut c_void, id: i32) -> *mut c_void {
    if idr.is_null() {
        return core::ptr::null_mut();
    }
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `idr` is verified non-null and valid per caller contract.
    let base = unsafe { (*idr).idr_base };
    if (id as u64) < (base as u64) {
        return core::ptr::null_mut();
    }
    let key = (id as u64) - (base as u64);
    // SAFETY: `idr` is valid and locked; accesses mutable slots of the embedded root.
    let slots = unsafe { radix_slots_mut(core::ptr::addr_of_mut!((*idr).idr_rt)) };
    if let Some(slot) = slots.slots.get_mut(&key) {
        let old = **slot;
        let new_val = if ptr.is_null() {
            IDR_RESERVED_ENTRY
        } else {
            ptr
        };
        **slot = new_val;
        if old == IDR_RESERVED_ENTRY {
            core::ptr::null_mut()
        } else {
            old
        }
    } else {
        core::ptr::null_mut()
    }
}

/// Linux `idr_remove()`: removes `id` from the idr.
///
/// # Safety
/// `idr` may be null (returning null); otherwise it must point to a live,
/// initialized IDR that is not being destroyed or reinitialized concurrently.
/// A returned value is an opaque caller-owned pointer, not freed or pinned by
/// this function; synchronize with existing readers before releasing it. The
/// owning subsystem must use its own lock/refcount protocol for entry lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_remove(idr: *mut Idr, id: i32) -> *mut c_void {
    if idr.is_null() {
        return core::ptr::null_mut();
    }
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `idr` is verified non-null and valid per caller contract.
    let base = unsafe { (*idr).idr_base };
    if (id as u64) < (base as u64) {
        return core::ptr::null_mut();
    }
    let key = (id as u64) - (base as u64);
    // SAFETY: `idr` is valid and locked; accesses mutable slots of the embedded root.
    let slots = unsafe { radix_slots_mut(core::ptr::addr_of_mut!((*idr).idr_rt)) };
    let old = slots
        .slots
        .remove(&key)
        .map_or(core::ptr::null_mut(), |slot| *slot);
    // SAFETY: `idr` is valid and locked; cleans up empty backing storage.
    unsafe { radix_release_if_empty(core::ptr::addr_of_mut!((*idr).idr_rt)) };
    if old == IDR_RESERVED_ENTRY {
        core::ptr::null_mut()
    } else {
        old
    }
}

/// Linux `idr_find()`.
///
/// # Safety
/// `idr` may be null; otherwise it must point to a live, initialized IDR for
/// the duration of the call. A returned pointer is not reference-counted or
/// pinned after the internal lock is released; callers must synchronize with
/// removal/destruction or acquire their own lifetime reference before use.
/// The internal storage lock does not protect the pointed-to object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_find(idr: *const Idr, id: c_ulong) -> *mut c_void {
    if idr.is_null() {
        return core::ptr::null_mut();
    }
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `idr` is verified non-null and valid per caller contract.
    let base = unsafe { (*idr).idr_base } as u64;
    if id < base {
        return core::ptr::null_mut();
    }
    let key = id - base;
    // SAFETY: `idr` is valid and locked; queries the embedded root.
    match unsafe { radix_slots_ref(core::ptr::addr_of!((*idr).idr_rt)) } {
        Some(slots) => {
            let ptr = slots
                .slots
                .get(&key)
                .map_or(core::ptr::null_mut(), |slot| **slot);
            if ptr == IDR_RESERVED_ENTRY {
                core::ptr::null_mut()
            } else {
                ptr
            }
        }
        None => core::ptr::null_mut(),
    }
}

/// Linux `idr_get_next()`: the first entry whose id is at least `*nextid`.
///
/// # Safety
/// `idr` must be non-null, aligned, and point to a live initialized IDR.
/// `nextid` must be non-null, aligned, and point to readable/writable `c_int`
/// storage that does not overlap the IDR. The returned entry is an unowned raw
/// pointer and is not pinned after the internal lock is released; callers
/// must synchronize against removal/destruction or retain their own reference.
/// The internal storage lock does not protect the pointed-to object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn idr_get_next(idr: *mut Idr, nextid: *mut c_int) -> *mut c_void {
    assert!(!idr.is_null() && !nextid.is_null());
    let _guard = RADIX_SLOTS_LOCK.lock();
    // SAFETY: `idr` is verified non-null and valid per caller contract.
    let base = unsafe { (*idr).idr_base } as u64;
    // SAFETY: `nextid` is verified non-null and readable per caller contract.
    let id = unsafe { *nextid } as u32 as u64;
    let start = id.saturating_sub(base);
    // SAFETY: `idr` is valid and locked; queries the embedded root.
    let Some(slots) = (unsafe { radix_slots_ref(core::ptr::addr_of!((*idr).idr_rt)) }) else {
        return core::ptr::null_mut();
    };
    let Some((&key, slot)) = slots
        .slots
        .range(start..)
        .find(|(_, slot)| !(***slot).is_null() && ***slot != IDR_RESERVED_ENTRY)
    else {
        return core::ptr::null_mut();
    };
    let found = key + base;
    if found > i32::MAX as u64 {
        return core::ptr::null_mut();
    }
    // SAFETY: `nextid` is verified non-null and writable per caller contract.
    unsafe { *nextid = found as c_int };
    **slot
}

/// Linux `idr_for_each()`.
///
/// # Safety
/// `idr` may be null (a no-op); otherwise it must point to a live, initialized
/// IDR that is not being destroyed or reinitialized during the call. The
/// implementation snapshots raw entry pointers under its internal lock and
/// invokes `f` after releasing that lock; the caller must keep each entry alive
/// until its callback runs and externally synchronize concurrent removal or
/// replacement. The callback must treat the entry as caller-owned and must not
/// dereference it unless that lifetime guarantee holds, nor invalidate a later
/// snapshot entry before its callback unless that entry has its own retained
/// reference. The internal lock protects only the IDR's slot storage.
pub unsafe fn idr_for_each<F>(idr: *mut Idr, mut f: F)
where
    F: FnMut(i32, *mut c_void),
{
    if idr.is_null() {
        return;
    }
    let entries: alloc::vec::Vec<(i32, *mut c_void)> = {
        let _guard = RADIX_SLOTS_LOCK.lock();
        // SAFETY: `idr` is verified non-null and valid per caller contract.
        let base = unsafe { (*idr).idr_base } as u64;
        // SAFETY: `idr` is valid and locked; queries the embedded root.
        match unsafe { radix_slots_ref(core::ptr::addr_of!((*idr).idr_rt)) } {
            Some(slots) => slots
                .slots
                .iter()
                .filter(|(_, slot)| !(***slot).is_null() && ***slot != IDR_RESERVED_ENTRY)
                .map(|(&k, slot)| ((k + base) as i32, **slot))
                .collect(),
            None => alloc::vec::Vec::new(),
        }
    };
    for (id, ptr) in entries {
        f(id, ptr);
    }
}

/// Linux `idr_preload()` compatibility stub; it allocates no state.
///
/// # Safety
/// This stub ignores `gfp` and accesses no memory, so there are no safety
/// preconditions.
#[inline]
pub unsafe extern "C" fn idr_preload(_gfp: u32) {}

/// Linux `idr_preload_end()` compatibility stub; it releases no state.
///
/// # Safety
/// This stub accesses no memory and does not depend on a preceding preload, so
/// there are no safety preconditions.
#[inline]
pub unsafe extern "C" fn idr_preload_end() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idr_unified_radix_operations() {
        // SAFETY: Test operates on a stack-allocated `Idr` initialized via `idr_init_base`
        // and destroyed at the end via `idr_destroy`.
        unsafe {
            let mut idr: Idr = core::mem::zeroed();
            idr_init_base(&mut idr, 1);
            assert!(idr_is_empty(&idr));

            let p1 = 0x1000usize as *mut c_void;
            let p2 = 0x2000usize as *mut c_void;
            let p3 = 0x3000usize as *mut c_void;

            let id1 = idr_alloc(&mut idr, p1, 1, 0);
            assert_eq!(id1, 1);
            let id2 = idr_alloc(&mut idr, p2, 1, 0);
            assert_eq!(id2, 2);
            let id3 = idr_alloc(&mut idr, p3, 5, 10);
            assert_eq!(id3, 5);

            assert!(!idr_is_empty(&idr));

            // idr_find
            assert_eq!(idr_find(&idr, 1), p1);
            assert_eq!(idr_find(&idr, 2), p2);
            assert_eq!(idr_find(&idr, 5), p3);
            assert!(idr_find(&idr, 3).is_null());

            // idr_get_next
            let mut next = 1;
            assert_eq!(idr_get_next(&mut idr, &mut next), p1);
            assert_eq!(next, 1);

            next = 2;
            assert_eq!(idr_get_next(&mut idr, &mut next), p2);
            assert_eq!(next, 2);

            next = 3;
            assert_eq!(idr_get_next(&mut idr, &mut next), p3);
            assert_eq!(next, 5);

            next = 6;
            assert!(idr_get_next(&mut idr, &mut next).is_null());

            // idr_for_each
            let mut visited = alloc::vec::Vec::new();
            idr_for_each(&mut idr, |id, ptr| {
                visited.push((id, ptr));
            });
            assert_eq!(visited, alloc::vec![(1, p1), (2, p2), (5, p3)]);

            // idr_replace
            let p1_new = 0x1111usize as *mut c_void;
            assert_eq!(idr_replace(&mut idr, p1_new, 1), p1);
            assert_eq!(idr_find(&idr, 1), p1_new);

            // idr_remove
            assert_eq!(idr_remove(&mut idr, 2), p2);
            assert!(idr_find(&idr, 2).is_null());

            idr_destroy(&mut idr);
            assert!(idr_is_empty(&idr));
        }
    }
}
