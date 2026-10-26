// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Iteration over the members of possibly nested fence containers (chains of
// chain links, each holding an array of fences), plus helpers that combine
// several fence trees into one fence.

use alloc::vec::Vec;
use core::{cmp::Ordering, ptr};

use crate::{
    dma_fence::{
        dma_fence_allocate_private_stub, dma_fence_context_alloc, dma_fence_get,
        dma_fence_is_later, dma_fence_is_signaled, dma_fence_put, dma_fence_timestamp,
    },
    dma_fence_array::{dma_fence_array_create_owned, dma_fence_array_first, dma_fence_array_next},
    dma_fence_chain::{dma_fence_chain_contained, dma_fence_chain_walk},
    types::DmaFence,
};

/// Cursor for walking the leaf fences under a head fence.
///
/// Start it with [`dma_fence_unwrap_first`] and advance it with
/// [`dma_fence_unwrap_next`]. The cursor holds one reference to the chain link
/// it is currently inside; the iteration consumes that reference as it moves.
#[repr(C)]
pub struct DmaFenceUnwrap {
    /// The chain link being visited (or the plain fence itself).
    pub chain: *mut DmaFence,
    /// The fence inside `chain` that holds the members: an array fence, or
    /// `chain`'s own contained fence when that is not an array.
    pub array: *mut DmaFence,
    /// Index of the member most recently returned from `array`.
    pub index: u32,
}

impl DmaFenceUnwrap {
    /// A cursor that is not iterating yet.
    pub const fn empty() -> Self {
        Self {
            chain: ptr::null_mut(),
            array: ptr::null_mut(),
            index: 0,
        }
    }
}

/// Point the cursor at the container held by its current chain link and
/// return that container's first member.
///
/// # Safety
/// `cursor.chain` must be null or a live fence reference owned by `cursor`.
unsafe fn enter_current_link(cursor: &mut DmaFenceUnwrap) -> *mut DmaFence {
    if cursor.chain.is_null() {
        cursor.array = ptr::null_mut();
        cursor.index = 0;
        return ptr::null_mut();
    }
    // SAFETY: the caller guarantees `cursor.chain` is live, and the contained
    // fence is kept alive by the chain link the cursor references.
    cursor.array = unsafe { dma_fence_chain_contained(cursor.chain) };
    cursor.index = 0;
    // SAFETY: `cursor.array` is null or the live contained fence above.
    unsafe { dma_fence_array_first(cursor.array) }
}

/// Start walking the leaf fences under `head`. Returns the first leaf, or null
/// when `head` is null. The cursor takes its own reference to `head`, so the
/// caller keeps its reference.
///
/// # Safety
/// `head` must be null or a live fence. `cursor` must not be mid-iteration:
/// starting over an active cursor would drop its reference without releasing
/// it.
pub unsafe fn dma_fence_unwrap_first(
    head: *mut DmaFence,
    cursor: &mut DmaFenceUnwrap,
) -> *mut DmaFence {
    // SAFETY: `head` is live or null as the caller guarantees; `dma_fence_get`
    // accepts null.
    cursor.chain = unsafe { dma_fence_get(head) };
    // SAFETY: `cursor.chain` now holds the reference taken above.
    unsafe { enter_current_link(cursor) }
}

/// Advance the cursor and return the next leaf fence, or null once every leaf
/// has been visited. Returned leaves are borrowed from the cursor's head.
///
/// # Safety
/// `cursor` must have been started with [`dma_fence_unwrap_first`] and not
/// finished since.
pub unsafe fn dma_fence_unwrap_next(cursor: &mut DmaFenceUnwrap) -> *mut DmaFence {
    cursor.index = cursor.index.wrapping_add(1);
    // SAFETY: `cursor.array` is null or the live container of the current link.
    let member = unsafe { dma_fence_array_next(cursor.array, cursor.index) };
    if !member.is_null() {
        return member;
    }
    // The current link is exhausted. Walking consumes the cursor's reference to
    // it and yields the next link, or null at the end of the chain.
    // SAFETY: the cursor owns `cursor.chain`, which the walk is allowed to
    // consume; the walk accepts null.
    cursor.chain = unsafe { dma_fence_chain_walk(cursor.chain) };
    // SAFETY: `cursor.chain` is null or a live link the cursor now owns.
    unsafe { enter_current_link(cursor) }
}

/// Total order used to group and deduplicate fences: by context ascending,
/// then newest first within a context.
///
/// # Safety
/// Both fences must be live.
unsafe fn compare_newest_first(a: *mut DmaFence, b: *mut DmaFence) -> Ordering {
    // SAFETY: both fences are live per the caller; the context field is only
    // read.
    let (context_a, context_b) = unsafe { ((*a).context, (*b).context) };
    match context_a.cmp(&context_b) {
        Ordering::Equal => {
            // SAFETY: both fences are live per the caller.
            if unsafe { dma_fence_is_later(a, b) } {
                Ordering::Less
            } else if unsafe { dma_fence_is_later(b, a) } {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        other => other,
    }
}

/// Keep only the newest fence of each context.
///
/// The slice is sorted so each context forms a run led by its newest fence.
/// Every fence that is not the leader of its run has its reference dropped.
/// The leaders are compacted to the front and their count is returned. Entries
/// past the returned count no longer carry references and must not be used.
///
/// # Safety
/// Every entry must be a live fence reference owned by the caller.
pub unsafe fn dma_fence_dedup_array(fences: &mut [*mut DmaFence]) -> usize {
    // SAFETY: every entry is live per the caller, so the comparator may read
    // them.
    fences.sort_by(|a, b| unsafe { compare_newest_first(*a, *b) });
    let mut kept = 0;
    for index in 0..fences.len() {
        let candidate = fences[index];
        // SAFETY: the candidate and the current leader are live fence refs.
        let same_context =
            kept > 0 && unsafe { (*candidate).context == (*fences[kept - 1]).context };
        if same_context {
            // SAFETY: the caller's reference to a shadowed fence is released here.
            unsafe { dma_fence_put(candidate) };
        } else {
            fences[kept] = candidate;
            kept += 1;
        }
    }
    kept
}

/// Release every reference in `pending`.
///
/// # Safety
/// Every entry must be an owned live fence reference.
unsafe fn release_all(pending: &mut Vec<*mut DmaFence>) {
    for fence in pending.drain(..) {
        // SAFETY: each entry is an owned reference per the caller.
        unsafe { dma_fence_put(fence) };
    }
}

/// Combine the leaf fences of every input into one fence that signals once
/// all of them have signaled.
///
/// Signaled leaves are dropped, and the newest signaled timestamp is kept. If
/// nothing is pending, a signaled stub carrying that timestamp is returned. A
/// single pending leaf is returned directly. Otherwise the pending leaves are
/// deduplicated and wrapped in a new array fence. The inputs are borrowed.
/// Returns null when memory for the merged result cannot be allocated.
///
/// # Safety
/// Every input must be a live fence.
pub unsafe fn dma_fence_unwrap_merge(fences: &[*mut DmaFence]) -> *mut DmaFence {
    let mut pending: Vec<*mut DmaFence> = Vec::new();
    let mut newest_signaled: i64 = 0;
    let mut out_of_memory = false;

    for &head in fences {
        let mut cursor = DmaFenceUnwrap::empty();
        // SAFETY: `head` is live per the caller.
        let mut leaf = unsafe { dma_fence_unwrap_first(head, &mut cursor) };
        while !leaf.is_null() {
            // SAFETY: leaves reached through the cursor are live.
            if unsafe { dma_fence_is_signaled(leaf) } {
                // SAFETY: as above.
                let stamp = unsafe { dma_fence_timestamp(leaf) };
                newest_signaled = newest_signaled.max(stamp);
            } else if !out_of_memory {
                if pending.try_reserve(1).is_ok() {
                    // SAFETY: the leaf is live; the pending list takes its own ref.
                    pending.push(unsafe { dma_fence_get(leaf) });
                } else {
                    out_of_memory = true;
                }
            }
            // Keep walking even after a failure so the cursor releases every
            // chain link it referenced.
            // SAFETY: the cursor was started above and has not finished.
            leaf = unsafe { dma_fence_unwrap_next(&mut cursor) };
        }
    }

    if out_of_memory {
        // SAFETY: the list holds only references this call took.
        unsafe { release_all(&mut pending) };
        return ptr::null_mut();
    }

    match pending.len() {
        0 => {
            // SAFETY: plain allocation of a signaled stub; no preconditions.
            return unsafe { dma_fence_allocate_private_stub(newest_signaled) };
        }
        1 => return pending[0],
        _ => {}
    }

    // SAFETY: every pending entry is a reference this call took.
    let kept = unsafe { dma_fence_dedup_array(&mut pending) };
    pending.truncate(kept);
    if kept == 1 {
        return pending[0];
    }

    // SAFETY: the list's references are handed to the array, which owns them
    // from here on.
    let array = unsafe { dma_fence_array_create_owned(pending, dma_fence_context_alloc(1), 1) };
    array.cast()
}
