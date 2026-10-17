// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! Fence arrays: a composite fence that signals once every member fence has
//! signaled. While members are outstanding the array's error field holds a
//! pending marker; the first non-zero member error replaces it, otherwise the
//! marker is cleared on completion.

use alloc::vec::Vec;
use core::{
    mem::{ManuallyDrop, MaybeUninit, offset_of},
    ptr,
    sync::atomic::{AtomicI32, Ordering},
};

use crate::{
    dma_fence::{
        DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT, DMA_FENCE_FLAG_INLINE_LOCK_BIT, dma_fence_add_callback,
        dma_fence_default_wait, dma_fence_free, dma_fence_get, dma_fence_is_signaled,
        dma_fence_put, dma_fence_set_deadline, dma_fence_signal,
    },
    memory::{atomic_read, atomic_sub_and_test, kref_init},
    types::{AtomicT, DmaFence, DmaFenceCb, DmaFenceOps, DmaFenceTimestamp, IrqWork, ListHead},
};

/// Error value stored in `base.error` while members are still outstanding.
const PENDING_ERROR: i32 = 1;

/// Per-member callback node. It lives inline after the array header and starts
/// with the callback record that the member fence links into its waiter list.
#[repr(C)]
struct DmaFenceArrayCallback {
    callback: DmaFenceCb,
    array: *mut DmaFenceArray,
}

/// Composite fence header. Its member callback nodes are stored inline after
/// the fixed fields, one per member, so one allocation covers the whole array.
#[repr(C)]
pub struct DmaFenceArray {
    pub base: DmaFence,
    num_fences: u32,
    num_pending: AtomicT,
    fences: *mut *mut DmaFence,
    work: IrqWork,
    callbacks: [MaybeUninit<DmaFenceArrayCallback>; 0],
}

/// Sets up the common fence header shared by array-style containers.
///
/// # Safety
/// `fence` must point to writable, suitably aligned storage for a `DmaFence`
/// that is not yet in use, and `ops` must stay valid for the fence's lifetime.
unsafe fn init_container_fence(
    fence: *mut DmaFence,
    ops: *const DmaFenceOps,
    context: u64,
    seqno: u64,
) {
    assert!(!fence.is_null() && !ops.is_null());
    // SAFETY: `fence` is non-null and writable per the contract; every field is
    // written here before the fence can be observed by anyone else.
    unsafe {
        crate::locks::spin_lock_init(&mut *ptr::addr_of_mut!((*fence).lock.inline_lock));
        (*fence).ops = ops.cast();
        (*fence).timestamp_union = DmaFenceTimestamp {
            cb_list: ManuallyDrop::new(ListHead {
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            }),
        };
        crate::list::INIT_LIST_HEAD(ptr::addr_of_mut!((*fence).timestamp_union.cb_list).cast());
        (*fence).context = context;
        (*fence).seqno = seqno;
        (*fence).flags = 1 << DMA_FENCE_FLAG_INLINE_LOCK_BIT;
        // SAFETY (refcount): `refcount` is part of the header initialized above.
        kref_init(ptr::addr_of_mut!((*fence).refcount));
        (*fence).error = 0;
    }
}

#[unsafe(no_mangle)]
pub static dma_fence_array_ops: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(array_driver_name),
    get_timeline_name: Some(array_timeline_name),
    enable_signaling: Some(array_enable_signaling),
    signaled: Some(array_signaled),
    wait: Some(dma_fence_default_wait),
    release: Some(array_release),
    set_deadline: Some(array_set_deadline),
};

/// # Safety
/// Only invoked through the fence ops table; ignores its argument.
unsafe extern "C" fn array_driver_name(_: *mut DmaFence) -> *const core::ffi::c_char {
    c"dma_fence_array".as_ptr()
}

/// # Safety
/// Only invoked through the fence ops table; ignores its argument.
unsafe extern "C" fn array_timeline_name(_: *mut DmaFence) -> *const core::ffi::c_char {
    c"unbound".as_ptr()
}

/// Returns the first member of a fence array, or `head` itself when it is an
/// ordinary fence. Null when `head` is null.
///
/// # Safety
/// `head` must be null or a live fence.
pub unsafe fn dma_fence_array_first(head: *mut DmaFence) -> *mut DmaFence {
    if head.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `head` is non-null and live per the contract, so its ops pointer
    // can be inspected.
    if unsafe { (*head).ops } != ptr::addr_of!(dma_fence_array_ops).cast() {
        return head;
    }
    // SAFETY: the ops table identifies `head` as a fence array.
    unsafe { member_at(head.cast::<DmaFenceArray>(), 0) }
}

/// Returns member `index` of a fence array, or null when `head` is not an
/// array or the index is past the last member.
///
/// # Safety
/// `head` must be null or a live fence.
pub unsafe fn dma_fence_array_next(head: *mut DmaFence, index: u32) -> *mut DmaFence {
    // SAFETY: `head` is checked for null before its ops pointer is read.
    if head.is_null() || unsafe { (*head).ops } != ptr::addr_of!(dma_fence_array_ops).cast() {
        return ptr::null_mut();
    }
    // SAFETY: the ops table identifies `head` as a fence array.
    unsafe { member_at(head.cast::<DmaFenceArray>(), index) }
}

/// Bounds-checked read of one member pointer.
///
/// # Safety
/// `array` must point to a live fence array.
unsafe fn member_at(array: *mut DmaFenceArray, index: u32) -> *mut DmaFence {
    // SAFETY: the bounds check keeps the offset inside the `fences` buffer,
    // which holds `num_fences` entries for a live array.
    unsafe {
        if index >= (*array).num_fences {
            return ptr::null_mut();
        }
        *(*array).fences.add(index as usize)
    }
}

/// Bytes needed for an array header plus one inline callback node per member.
fn allocation_size(num_fences: u32) -> Option<usize> {
    (num_fences as usize)
        .checked_mul(core::mem::size_of::<DmaFenceArrayCallback>())?
        .checked_add(core::mem::size_of::<DmaFenceArray>())
}

/// Creates a fence array that takes ownership of the supplied member references
/// and of the pointer buffer. Returns null on invalid arguments or allocation
/// failure, in which case nothing is consumed.
///
/// # Safety
/// `fences` must point to `num_fences` initialized live fence pointers in a
/// kernel-allocated array. Ownership of that array and each member reference
/// transfers to the new array on success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_array_create(
    num_fences: u32,
    fences: *mut *mut DmaFence,
    context: u64,
    seqno: u32,
) -> *mut DmaFenceArray {
    if num_fences == 0 || fences.is_null() {
        return ptr::null_mut();
    }
    let Some(bytes) = allocation_size(num_fences) else {
        return ptr::null_mut();
    };
    let array = crate::memory::kzalloc(bytes, crate::config::GFP_KERNEL).cast::<DmaFenceArray>();
    if array.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `array` is a fresh zeroed allocation large enough for the header
    // and `num_fences` inline callback nodes, so every field write is in bounds.
    unsafe {
        (*array).num_fences = num_fences;
        (*array).num_pending.counter = num_fences as i32;
        (*array).fences = fences;
        crate::irq_work::init_irq_work(ptr::addr_of_mut!((*array).work), array_deferred_work);
        init_container_fence(
            ptr::addr_of_mut!((*array).base),
            ptr::addr_of!(dma_fence_array_ops),
            context,
            u64::from(seqno),
        );
        (*array).base.error = PENDING_ERROR;
    }
    array
}

/// Puts every reference in `fences`.
///
/// # Safety
/// Each entry must be a live fence reference owned by the caller, put once.
unsafe fn put_all(fences: &[*mut DmaFence]) {
    for &fence in fences {
        // SAFETY: each entry is an owned live reference per the contract.
        unsafe { dma_fence_put(fence) };
    }
}

/// Builds a fence array from a vector of owned fence references.
///
/// The array ABI requires a kernel-allocated pointer buffer, so the references
/// are copied into one. On success the new array owns them; on failure they are
/// all put and null is returned.
///
/// # Safety
/// Every entry in `fences` must be a live fence reference owned by the caller.
pub unsafe fn dma_fence_array_create_owned(
    fences: Vec<*mut DmaFence>,
    context: u64,
    seqno: u32,
) -> *mut DmaFenceArray {
    let Ok(num_fences) = u32::try_from(fences.len()) else {
        // SAFETY: the references are owned by this call and not used afterwards.
        unsafe { put_all(&fences) };
        return ptr::null_mut();
    };
    if num_fences == 0 {
        return ptr::null_mut();
    }

    // SAFETY: plain allocation; a null result is handled below before any use.
    let storage = unsafe {
        crate::memory::kmalloc_array(
            fences.len(),
            core::mem::size_of::<*mut DmaFence>(),
            crate::config::GFP_KERNEL,
        )
        .cast::<*mut DmaFence>()
    };
    if storage.is_null() {
        // SAFETY: as above, the references are owned by this call.
        unsafe { put_all(&fences) };
        return ptr::null_mut();
    }
    // SAFETY: `storage` has room for `fences.len()` pointers and is a fresh
    // allocation, so it cannot overlap the source slice.
    unsafe { ptr::copy_nonoverlapping(fences.as_ptr(), storage, fences.len()) };

    // SAFETY: `storage` holds `num_fences` owned references and is kernel-allocated,
    // which is exactly what `dma_fence_array_create` requires.
    let array = unsafe { dma_fence_array_create(num_fences, storage, context, seqno) };
    if array.is_null() {
        // SAFETY: creation failed, so the buffer and references are still ours to release.
        unsafe {
            crate::memory::kfree(storage);
            put_all(&fences);
        }
    }
    array
}

/// Irq-work body that finishes a composite fence once its last member has
/// reported in.
///
/// # Safety
/// `work` must be the `work` field of a live array scheduled by
/// `array_member_signaled`.
unsafe extern "C" fn array_deferred_work(work: *mut IrqWork) {
    // SAFETY: `work` is embedded in a live array, so the offset subtraction
    // recovers the enclosing header.
    unsafe {
        let array = work
            .cast::<u8>()
            .sub(offset_of!(DmaFenceArray, work))
            .cast::<DmaFenceArray>();
        clear_pending_error(array);
        dma_fence_signal(ptr::addr_of_mut!((*array).base));
        dma_fence_put(ptr::addr_of_mut!((*array).base));
    }
}

/// Member callback: records the member's error and counts it off. The last
/// member schedules the deferred completion; others drop their array reference.
///
/// # Safety
/// `callback` must be the callback field of a live `DmaFenceArrayCallback`, and
/// `member` must be the fence that fired it.
unsafe extern "C" fn array_member_signaled(member: *mut DmaFence, callback: *mut DmaFenceCb) {
    // SAFETY: `callback` is the first field of an inline callback node, so the
    // cast is valid; the node points back at its live array.
    unsafe {
        let node = callback.cast::<DmaFenceArrayCallback>();
        let array = (*node).array;
        record_member_error(array, (*member).error);
        if atomic_sub_and_test(1, &mut (*array).num_pending) {
            crate::irq_work::irq_work_queue(ptr::addr_of_mut!((*array).work));
        } else {
            dma_fence_put(ptr::addr_of_mut!((*array).base));
        }
    }
}

/// Arms a callback on every member. Members that have already signaled are
/// counted off immediately. Returns `false` when all members are done, so the
/// caller signals the array directly.
///
/// # Safety
/// `fence` must be a live fence array owned by the caller for this call.
unsafe extern "C" fn array_enable_signaling(fence: *mut DmaFence) -> bool {
    // SAFETY: `fence` is a live array per the ops-table contract. Each inline
    // callback slot is within the allocation sized in `dma_fence_array_create`.
    unsafe {
        let array = fence.cast::<DmaFenceArray>();
        for index in 0..(*array).num_fences {
            let slot = ptr::addr_of_mut!((*array).callbacks)
                .cast::<DmaFenceArrayCallback>()
                .add(index as usize);
            (*slot).array = array;
            // Each armed callback keeps the array alive until it fires.
            dma_fence_get(ptr::addr_of_mut!((*array).base));
            let member = *(*array).fences.add(index as usize);
            if dma_fence_add_callback(
                member,
                ptr::addr_of_mut!((*slot).callback),
                array_member_signaled,
            ) != 0
            {
                // The member already signaled, so no callback will run for it.
                record_member_error(array, (*member).error);
                dma_fence_put(ptr::addr_of_mut!((*array).base));
                if atomic_sub_and_test(1, &mut (*array).num_pending) {
                    clear_pending_error(array);
                    return false;
                }
            }
        }
        true
    }
}

/// Reports whether the composite fence is complete, either from the armed
/// counter or, before signaling was enabled, by inspecting each member.
///
/// # Safety
/// `fence` must be a live fence array.
unsafe extern "C" fn array_signaled(fence: *mut DmaFence) -> bool {
    // SAFETY: `fence` is a live array; member pointers are read within bounds.
    unsafe {
        let array = fence.cast::<DmaFenceArray>();
        let pending = atomic_read(&(*array).num_pending);
        if crate::bits::test_bit(DMA_FENCE_FLAG_ENABLE_SIGNAL_BIT, &(*fence).flags) {
            // Callbacks are armed, so the counter is authoritative.
            if pending <= 0 {
                clear_pending_error(array);
                return true;
            }
            return false;
        }

        let mut remaining = pending;
        for index in 0..(*array).num_fences {
            let member = *(*array).fences.add(index as usize);
            if dma_fence_is_signaled(member) {
                remaining -= 1;
                if remaining <= 0 {
                    clear_pending_error(array);
                    return true;
                }
            }
        }
        false
    }
}

/// Stores a member's error as the array's error, but only while the array is
/// still pending and has not already recorded an error. The first error wins.
///
/// # Safety
/// `array` must be a live fence array.
unsafe fn record_member_error(array: *mut DmaFenceArray, error: i32) {
    if error == 0 {
        return;
    }
    // SAFETY: `error` is a plain i32 field inside a live array header, so it is a
    // valid atomic location for the duration of this call.
    unsafe {
        let _ = AtomicI32::from_ptr(ptr::addr_of_mut!((*array).base.error)).compare_exchange(
            PENDING_ERROR,
            error,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// Turns the pending marker into a clean "no error" state. A recorded member
/// error is left untouched.
///
/// # Safety
/// `array` must be a live fence array.
unsafe fn clear_pending_error(array: *mut DmaFenceArray) {
    // SAFETY: as in `record_member_error`, the field is a valid atomic location.
    unsafe {
        let _ = AtomicI32::from_ptr(ptr::addr_of_mut!((*array).base.error)).compare_exchange(
            PENDING_ERROR,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// Final release: drops the member references, frees the pointer buffer, and
/// releases the header.
///
/// # Safety
/// `fence` must be the array whose refcount has reached zero.
unsafe extern "C" fn array_release(fence: *mut DmaFence) {
    // SAFETY: the refcount reached zero, so the members and buffer are owned
    // solely by this array and are released exactly once here.
    unsafe {
        let array = fence.cast::<DmaFenceArray>();
        for index in 0..(*array).num_fences {
            dma_fence_put(*(*array).fences.add(index as usize));
        }
        crate::memory::kfree((*array).fences);
        dma_fence_free(fence);
    }
}

/// Forwards a deadline hint to every member.
///
/// # Safety
/// `fence` must be a live fence array.
unsafe extern "C" fn array_set_deadline(fence: *mut DmaFence, deadline: i64) {
    // SAFETY: `fence` is a live array; member pointers are read within bounds.
    unsafe {
        let array = fence.cast::<DmaFenceArray>();
        for index in 0..(*array).num_fences {
            dma_fence_set_deadline(*(*array).fences.add(index as usize), deadline);
        }
    }
}

const _: [(); 32] = [(); core::mem::size_of::<DmaFenceArrayCallback>()];
const _: [(); 112] = [(); core::mem::size_of::<DmaFenceArray>()];
const _: [(); 80] = [(); core::mem::offset_of!(DmaFenceArray, work)];
const _: [(); 112] = [(); core::mem::offset_of!(DmaFenceArray, callbacks)];
