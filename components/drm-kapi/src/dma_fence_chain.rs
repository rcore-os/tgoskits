// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors.
//! Timeline fence chains.
//!
//! A chain link pairs one timeline point (`seqno`) with the fence that will
//! signal that point, and points back at the earlier link. Links whose fence
//! has already signaled are unlinked lazily while a walk advances through the
//! timeline, so a long-lived timeline does not keep every past point alive.

use core::{
    ffi::c_void,
    mem::{ManuallyDrop, offset_of},
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{
    dma_fence::{
        DMA_FENCE_FLAG_INLINE_LOCK_BIT, dma_fence_add_callback, dma_fence_default_wait,
        dma_fence_free, dma_fence_get, dma_fence_get_rcu, dma_fence_is_signaled, dma_fence_put,
        dma_fence_set_deadline, dma_fence_signal,
    },
    types::{
        DmaFence, DmaFenceCb, DmaFenceChain, DmaFenceOps, DmaFenceTimestamp, IrqWork, ListHead,
        Spinlock,
    },
};

/// Flag bit marking a fence whose sequence numbers span the full 64-bit range.
const SEQNO64_FLAG_BIT: u32 = 2;

#[cfg(not(test))]
use crate::dma_fence::dma_fence_context_alloc;

#[cfg(test)]
mod test_mocks {
    use core::sync::atomic::{AtomicU64, Ordering};

    static NEXT_CONTEXT: AtomicU64 = AtomicU64::new(100);

    /// Test stand-in for the kernel context allocator.
    ///
    /// # Safety
    /// Has no preconditions; declared `unsafe` to match the production
    /// signature it replaces.
    pub unsafe fn dma_fence_context_alloc(_num: u32) -> u64 {
        NEXT_CONTEXT.fetch_add(1, Ordering::Relaxed)
    }
}
#[cfg(test)]
use test_mocks::*;

/// Reports whether `fence` is a chain link, judged by its ops table.
///
/// # Safety
/// `fence` must be null or point to a fence header that stays readable for
/// the duration of the call.
#[inline]
unsafe fn is_chain(fence: *const DmaFence) -> bool {
    // SAFETY: a non-null pointer is checked for its ops table; the caller
    // guarantees a null or readable fence header.
    unsafe {
        !fence.is_null() && (*fence).ops == ptr::addr_of!(dma_fence_chain_ops).cast::<c_void>()
    }
}

/// Compares two sequence numbers using the width the fence was created with.
///
/// # Safety
/// `fence` must point to a readable fence header.
#[inline]
unsafe fn is_later_seqno(fence: *const DmaFence, newer: u64, older: u64) -> bool {
    // SAFETY: the caller guarantees `fence` is readable, so its flag word can
    // be read to select the comparison width.
    if crate::bits::test_bit(SEQNO64_FLAG_BIT, unsafe { &(*fence).flags }) {
        newer > older
    } else {
        (newer as u32).wrapping_sub(older as u32) as i32 > 0
    }
}

/// Takes a counted reference to the link currently stored in `chain.prev`.
/// Returns null when there is no previous link or it has already been freed.
///
/// # Safety
/// `chain` must point to a live chain link.
unsafe fn load_prev_ref(chain: *mut DmaFenceChain) -> *mut DmaFence {
    crate::rcu::rcu_read_lock();
    // SAFETY: `chain` is live, so its `prev` slot is a valid atomic location;
    // the RCU read section keeps the loaded link from being freed before the
    // reference is taken.
    let prev =
        unsafe { AtomicPtr::from_ptr(ptr::addr_of_mut!((*chain).prev)).load(Ordering::Acquire) };
    // SAFETY: the RCU read-side section above keeps `prev` allocated.
    let taken = unsafe { dma_fence_get_rcu(prev) };
    crate::rcu::rcu_read_unlock();
    taken
}

/// Initialize caller-owned chain storage and transfer optional fence refs.
///
/// # Safety
/// `chain` must point to writable, properly aligned storage not already in
/// use. Non-null `prev` and `fence` must be live references whose ownership
/// is transferred to the chain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_chain_init(
    chain: *mut DmaFenceChain,
    prev: *mut DmaFence,
    fence: *mut DmaFence,
    mut seqno: u64,
) {
    assert!(!chain.is_null());
    // SAFETY: `chain` is non-null and writable per the contract, and every
    // field is written before the chain is shared. `prev` is read only when it
    // is a chain link, which the caller guarantees is live.
    unsafe {
        let prev_is_chain = is_chain(prev);
        let mut prev_seqno = 0;
        let context = if prev_is_chain && is_later_seqno(prev, seqno, (*prev).seqno) {
            // Extending the timeline past its last point keeps the same context.
            prev_seqno = (*prev).seqno;
            (*prev).context
        } else {
            // Out-of-order points start a new context; never sort before the predecessor.
            if prev_is_chain {
                seqno = seqno.max((*prev).seqno);
            }
            dma_fence_context_alloc(1)
        };

        (*chain).prev = prev;
        (*chain).fence = fence;
        (*chain).prev_seqno = prev_seqno;

        let base = ptr::addr_of_mut!((*chain).base);
        (*base).lock.inline_lock = Spinlock::new();
        (*base).ops = ptr::addr_of!(dma_fence_chain_ops).cast::<c_void>();
        (*base).timestamp_union = DmaFenceTimestamp {
            cb_list: ManuallyDrop::new(ListHead {
                next: ptr::null_mut(),
                prev: ptr::null_mut(),
            }),
        };
        crate::list::INIT_LIST_HEAD(ptr::addr_of_mut!((*base).timestamp_union.cb_list).cast());
        (*base).context = context;
        (*base).seqno = seqno;
        (*base).flags = (1 << SEQNO64_FLAG_BIT) | (1 << DMA_FENCE_FLAG_INLINE_LOCK_BIT);
        (*base).error = 0;
        // SAFETY (refcount): the refcount field lives inside `base`, which was
        // fully initialized just above.
        crate::memory::kref_init(ptr::addr_of_mut!((*base).refcount));
    }
}

/// Returns the fence that a chain link directly wraps, or `fence` itself when
/// it is not a chain link. The result is null for a link with no fence yet.
///
/// # Safety
/// `fence` must be null or a live initialized fence reference.
pub unsafe fn dma_fence_chain_contained(fence: *mut DmaFence) -> *mut DmaFence {
    // SAFETY: the caller guarantees `fence` is null or a live fence; a chain
    // link is only reinterpreted after its ops table matches.
    unsafe {
        if is_chain(fence) {
            (*fence.cast::<DmaFenceChain>()).fence
        } else {
            fence
        }
    }
}

#[unsafe(no_mangle)]
pub static dma_fence_chain_ops: DmaFenceOps = DmaFenceOps {
    get_driver_name: Some(chain_driver_name),
    get_timeline_name: Some(chain_timeline_name),
    enable_signaling: Some(chain_enable_signaling),
    signaled: Some(chain_signaled),
    wait: Some(dma_fence_default_wait),
    release: Some(chain_release),
    set_deadline: Some(chain_set_deadline),
};

/// # Safety
/// Only invoked through the fence ops table; ignores its argument.
unsafe extern "C" fn chain_driver_name(_: *mut DmaFence) -> *const core::ffi::c_char {
    c"dma_fence_chain".as_ptr()
}

/// # Safety
/// Only invoked through the fence ops table; ignores its argument.
unsafe extern "C" fn chain_timeline_name(_: *mut DmaFence) -> *const core::ffi::c_char {
    c"unbound".as_ptr()
}

/// Arms signaling on every pending member of the timeline. Each armed child
/// holds a callback that schedules deferred work on the chain. Returns `false`
/// when no member can still deliver a signal, so the caller signals directly.
///
/// # Safety
/// `fence` must be a live chain link owned by the caller for this call.
unsafe extern "C" fn chain_enable_signaling(fence: *mut DmaFence) -> bool {
    // SAFETY: `fence` is a live chain link per the ops-table contract. The
    // chain's `work` storage is reused as the callback node, which is sound
    // because it lives at a fixed offset inside the chain and stays allocated
    // until the chain is released.
    unsafe {
        let chain = fence.cast::<DmaFenceChain>();
        let base = ptr::addr_of_mut!((*chain).base);
        let node = ptr::addr_of_mut!((*chain).work).cast::<DmaFenceCb>();

        // Keep the chain alive for as long as any child callback is armed.
        dma_fence_get(base);

        let mut cursor = dma_fence_get(fence);
        while !cursor.is_null() {
            let member = dma_fence_chain_contained(cursor);
            if !member.is_null() {
                dma_fence_get(member);
                if dma_fence_add_callback(member, node, chain_member_signaled) == 0 {
                    // The callback now owns `member`'s reference and the
                    // armed state keeps `base` alive; drop only our cursor.
                    dma_fence_put(cursor);
                    return true;
                }
                dma_fence_put(member);
            }
            cursor = dma_fence_chain_walk(cursor);
        }

        dma_fence_put(base);
        false
    }
}

/// Callback run when a child fence signals. Defers the follow-up work to
/// irq-work context and releases the reference the callback was given.
///
/// # Safety
/// `callback` must be the `work` storage of a live chain link, and `fence` must
/// carry the reference handed to this callback.
unsafe extern "C" fn chain_member_signaled(fence: *mut DmaFence, callback: *mut DmaFenceCb) {
    // SAFETY: `callback` points at the `work` field embedded in a live chain, so
    // subtracting the field offset yields the enclosing chain.
    unsafe {
        let chain = callback
            .cast::<u8>()
            .sub(offset_of!(DmaFenceChain, work))
            .cast::<DmaFenceChain>();
        crate::irq_work::init_irq_work(ptr::addr_of_mut!((*chain).work), chain_deferred_work);
        crate::irq_work::irq_work_queue(ptr::addr_of_mut!((*chain).work));
        dma_fence_put(fence);
    }
}

/// Irq-work body: re-arm signaling for the remaining members, or signal the
/// chain if nothing is left to wait on.
///
/// # Safety
/// `work` must be the `work` field of a live chain link scheduled by
/// `chain_member_signaled`.
unsafe extern "C" fn chain_deferred_work(work: *mut IrqWork) {
    // SAFETY: `work` is the `work` field of a live chain, per the contract.
    unsafe {
        let chain = work
            .cast::<u8>()
            .sub(offset_of!(DmaFenceChain, work))
            .cast::<DmaFenceChain>();
        let base = ptr::addr_of_mut!((*chain).base);
        if !chain_enable_signaling(base) {
            dma_fence_signal(base);
        }
        dma_fence_put(base);
    }
}

/// Reports whether every point from this link back to the first pending one
/// has signaled.
///
/// # Safety
/// `fence` must be null or a live fence reference.
unsafe extern "C" fn chain_signaled(fence: *mut DmaFence) -> bool {
    // SAFETY: each cursor is a counted reference obtained from `dma_fence_get`
    // or `dma_fence_chain_walk`, released on every exit path.
    unsafe {
        let mut cursor = dma_fence_get(fence);
        while !cursor.is_null() {
            let member = dma_fence_chain_contained(cursor);
            if member.is_null() || !dma_fence_is_signaled(member) {
                dma_fence_put(cursor);
                return false;
            }
            cursor = dma_fence_chain_walk(cursor);
        }
        true
    }
}

/// Final release of a chain link. Unlinks uniquely owned predecessor links
/// iteratively, so deep timelines do not recurse through their release path.
///
/// # Safety
/// `fence` must be the chain link whose refcount has reached zero.
unsafe extern "C" fn chain_release(fence: *mut DmaFence) {
    // SAFETY: the refcount reached zero, so nothing else reaches this link
    // except through predecessor slots we are about to take apart.
    unsafe {
        let chain = fence.cast::<DmaFenceChain>();
        let mut prev = (*chain).prev;
        while is_chain(prev) && crate::memory::kref_read(&(*prev).refcount) <= 1 {
            let link = prev.cast::<DmaFenceChain>();
            let beyond = (*link).prev;
            // Detach the predecessor so its own release does not cascade.
            (*chain).prev = beyond;
            (*link).prev = ptr::null_mut();
            dma_fence_put(prev);
            prev = beyond;
        }
        dma_fence_put(prev);
        dma_fence_put((*chain).fence);
        dma_fence_free(fence);
    }
}

/// Propagates a deadline hint to every pending member of the timeline.
///
/// # Safety
/// `fence` must be null or a live fence reference.
unsafe extern "C" fn chain_set_deadline(fence: *mut DmaFence, deadline: i64) {
    // SAFETY: cursors follow the same counted-reference discipline as
    // `chain_signaled`.
    unsafe {
        let mut cursor = dma_fence_get(fence);
        while !cursor.is_null() {
            let member = dma_fence_chain_contained(cursor);
            if !member.is_null() {
                dma_fence_set_deadline(member, deadline);
            }
            cursor = dma_fence_chain_walk(cursor);
        }
    }
}

/// Consumes the caller's reference to `fence` and steps to the earliest link
/// that has not signaled yet, unlinking signaled links on the way. The returned
/// pointer, when non-null, carries a reference for the caller.
///
/// # Safety
/// `fence` must be null or a live owned fence reference that the caller may
/// relinquish to this function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_chain_walk(fence: *mut DmaFence) -> *mut DmaFence {
    // SAFETY: the caller hands over one owned reference, which is released
    // exactly once below; chain links are only touched after the ops check.
    unsafe {
        if !is_chain(fence) {
            dma_fence_put(fence);
            return ptr::null_mut();
        }
        let chain = fence.cast::<DmaFenceChain>();
        let pending = prune_signaled_prefix(chain);
        dma_fence_put(fence);
        pending
    }
}

/// Repeatedly inspects the `prev` slot of `chain`. Signaled links are swapped
/// out for their own predecessor; the first pending link is returned with a
/// reference for the caller. Returns null once nothing pending remains.
///
/// # Safety
/// `chain` must be a live chain link.
unsafe fn prune_signaled_prefix(chain: *mut DmaFenceChain) -> *mut DmaFence {
    // SAFETY: `chain` is live, and every reference held in a local is either
    // returned to the caller or released before the loop continues.
    unsafe {
        loop {
            let candidate = load_prev_ref(chain);
            if candidate.is_null() {
                return ptr::null_mut();
            }

            let replacement = if is_chain(candidate) {
                let link = candidate.cast::<DmaFenceChain>();
                let member = (*link).fence;
                if member.is_null() || !dma_fence_is_signaled(member) {
                    return candidate;
                }
                // The link is done; splice in its own predecessor.
                load_prev_ref(link)
            } else if !dma_fence_is_signaled(candidate) {
                return candidate;
            } else {
                ptr::null_mut()
            };

            let swapped = AtomicPtr::from_ptr(ptr::addr_of_mut!((*chain).prev))
                .compare_exchange(candidate, replacement, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
            if swapped {
                // The slot's old reference is gone; the replacement now owns the slot's ref.
                dma_fence_put(candidate);
            } else {
                dma_fence_put(replacement);
            }
            dma_fence_put(candidate);
        }
    }
}

/// Allocates uninitialized storage sized for one chain link.
pub fn dma_fence_chain_alloc<T>() -> *mut T {
    crate::memory::kmalloc(
        core::mem::size_of::<DmaFenceChain>(),
        crate::config::GFP_KERNEL,
    )
    .cast()
}

/// Frees chain storage returned by `dma_fence_chain_alloc`.
///
/// # Safety
/// `chain` must be null or point to a live allocation produced by
/// `dma_fence_chain_alloc` that is no longer used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_chain_free(chain: *mut c_void) {
    if !chain.is_null() {
        // SAFETY: non-null `chain` is a live allocation from `dma_fence_chain_alloc`,
        // unused per the contract, so handing it to `kfree` is sound.
        unsafe { crate::memory::kfree(chain) };
    }
}

/// Finds the earliest fence on `fence`'s timeline that covers `seqno`, updating
/// `*fence` in place. A null result means the point has already signaled.
///
/// # Safety
/// `fence` must point to writable storage containing a null pointer or a live
/// owned fence reference; the referenced chain must remain valid during the
/// search.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dma_fence_chain_find_seqno(fence: *mut *mut DmaFence, seqno: u64) -> i32 {
    if seqno == 0 {
        return 0;
    }
    // SAFETY: `fence` is checked for null before its pointee is read.
    if fence.is_null() || unsafe { (*fence).is_null() } {
        return -crate::config::EINVAL;
    }
    // SAFETY: the pointer and its owned reference satisfy the caller contract.
    unsafe { dma_fence_chain_find_seqno_walk(fence, seqno) }
}

/// Performs the timeline search for `dma_fence_chain_find_seqno` once the
/// arguments have been validated.
///
/// # Safety
/// `fence` must point to writable storage containing a live owned fence
/// reference, and that reference may be consumed or replaced by the walk.
pub unsafe fn dma_fence_chain_find_seqno_walk(fence: *mut *mut DmaFence, seqno: u64) -> i32 {
    // SAFETY: `*fence` is a live owned reference per the contract; it is
    // released below after the cursor has taken its own reference.
    unsafe {
        let start = *fence;
        if !is_chain(start) || (*start).seqno < seqno {
            return -crate::config::EINVAL;
        }
        let timeline = (*start).context;
        let mut cursor = dma_fence_get(start);
        while is_chain(cursor) {
            let link = cursor.cast::<DmaFenceChain>();
            // Stop at the first link whose predecessor no longer covers `seqno`
            // or that belongs to another timeline.
            if (*cursor).context != timeline || (*link).prev_seqno < seqno {
                break;
            }
            cursor = dma_fence_chain_walk(cursor);
        }
        *fence = cursor;
        dma_fence_put(start);
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_init_links_and_seqno_ordering() {
        let mut first_storage = core::mem::MaybeUninit::<DmaFenceChain>::zeroed();
        let first = first_storage.as_mut_ptr();
        unsafe {
            dma_fence_chain_init(first, ptr::null_mut(), ptr::null_mut(), 10);
            assert!((*first).prev.is_null());
            assert_eq!((*first).prev_seqno, 0);
            assert_eq!((*first).base.seqno, 10);
            assert!((*first).base.context >= 100);
        }

        // A later point on the same timeline inherits the context and records
        // its predecessor's seqno.
        let mut second_storage = core::mem::MaybeUninit::<DmaFenceChain>::zeroed();
        let second = second_storage.as_mut_ptr();
        unsafe {
            let first_base = ptr::addr_of_mut!((*first).base);
            dma_fence_chain_init(second, first_base, ptr::null_mut(), 15);
            assert_eq!((*second).prev, first_base);
            assert_eq!((*second).prev_seqno, 10);
            assert_eq!((*second).base.seqno, 15);
            assert_eq!((*second).base.context, (*first).base.context);
        }

        // An earlier point starts a fresh context and is raised to the
        // predecessor's seqno.
        let mut third_storage = core::mem::MaybeUninit::<DmaFenceChain>::zeroed();
        let third = third_storage.as_mut_ptr();
        unsafe {
            let second_base = ptr::addr_of_mut!((*second).base);
            dma_fence_chain_init(third, second_base, ptr::null_mut(), 5);
            assert_eq!((*third).prev, second_base);
            assert_eq!((*third).prev_seqno, 0);
            assert_eq!((*third).base.seqno, 15);
            assert_ne!((*third).base.context, (*second).base.context);
        }
    }
}
