//! Pinned coroutine allocation and lifetime accounting.

use alloc::{
    alloc::{Layout, dealloc},
    sync::Arc,
};
use core::{
    cell::{Cell, UnsafeCell},
    future::Future,
    marker::PhantomPinned,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
    task::{Context, Poll},
};

use super::SharedExecutor;
use crate::{ThreadId, reclaim::DeferredReclaimNode, runtime::task_runtime};

pub(super) const RUN_QUEUED: usize = 1 << 0;
pub(super) const POLLING: usize = 1 << 1;
pub(super) const COMPLETE: usize = 1 << 2;
const FUTURE_EMPTY: usize = 1 << 3;

const REFCOUNT_OVERFLOW_INVARIANT: u32 = 0x4558_0001;
const EARLY_RECLAIM_INVARIANT: u32 = 0x4558_0002;

type PollFuture = unsafe fn(*mut CoroutineHeader, &mut Context<'_>) -> Poll<()>;
type DropFuture = unsafe fn(*mut CoroutineHeader);
type Deallocate = unsafe fn(*mut CoroutineHeader);

/// Generation-bearing identity of one coroutine owned by a local executor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CoroutineId {
    owner_thread: ThreadId,
    generation: u64,
}

impl CoroutineId {
    pub(super) const fn new(owner_thread: ThreadId, generation: u64) -> Self {
        Self {
            owner_thread,
            generation,
        }
    }

    /// Returns the scheduler thread that owns this coroutine.
    pub const fn owner_thread(self) -> ThreadId {
        self.owner_thread
    }

    /// Returns the executor-local allocation generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Pinned header addressed directly by the coroutine's custom raw waker.
///
/// All fields touched outside the owner thread are atomic or immutable. The
/// future itself follows this header in a private allocation and is inaccessible
/// to raw waker operations.
#[repr(C)]
pub struct CoroutineHeader {
    reclaim: DeferredReclaimNode,
    id: CoroutineId,
    pub(super) state: AtomicUsize,
    references: AtomicUsize,
    executor: Arc<SharedExecutor>,
    ready_next: AtomicPtr<Self>,
    owner_next: Cell<*mut Self>,
    poll_future: PollFuture,
    drop_future: DropFuture,
    deallocate: Deallocate,
    _pin: PhantomPinned,
}

impl CoroutineHeader {
    /// Returns this allocation's generation-bearing identity.
    pub const fn id(&self) -> CoroutineId {
        self.id
    }

    /// Returns the owner thread embedded in every waker for this coroutine.
    pub const fn owner_thread(&self) -> ThreadId {
        self.id.owner_thread()
    }

    /// Polls the allocation-specific future.
    ///
    /// # Safety
    ///
    /// The caller must be the executor owner, hold a live allocation reference,
    /// and guarantee that no future poll or drop overlaps this call.
    pub(super) unsafe fn poll_raw(header: *mut Self, context: &mut Context<'_>) -> Poll<()> {
        let poll_future = unsafe {
            // Copying the function pointer through the original allocation
            // pointer does not widen a reference over the containing coroutine.
            core::ptr::addr_of!((*header).poll_future).read()
        };
        unsafe {
            // Construction installs the allocation-specific function and the
            // caller guarantees exclusive owner-thread access to the future.
            poll_future(header, context)
        }
    }

    /// Drops the allocation-specific future in place.
    ///
    /// # Safety
    ///
    /// The caller must be the executor owner, hold a live allocation reference,
    /// and call this exactly once after completion or owner shutdown.
    pub(super) unsafe fn drop_future_raw(header: *mut Self) {
        let drop_future = unsafe {
            // Preserve allocation provenance for the type-erased future slot.
            core::ptr::addr_of!((*header).drop_future).read()
        };
        unsafe {
            // Completion and cancellation serialize this operation on the owner.
            drop_future(header);
        }
    }

    /// Frees a zero-reference coroutine allocation in task context.
    ///
    /// # Safety
    ///
    /// `header` must be detached from the task-system reclaim inbox, have zero
    /// references, and have had its future emptied by the owner.
    unsafe fn deallocate_raw(header: *mut Self) {
        let state = unsafe { (*header).state.load(Ordering::Acquire) };
        if state & (COMPLETE | FUTURE_EMPTY) != (COMPLETE | FUTURE_EMPTY) {
            task_runtime::fatal_invariant(EARLY_RECLAIM_INVARIANT, unsafe {
                (*header).id.generation() as usize
            });
        }
        let deallocate = unsafe {
            // The detached node remains valid until the callback is copied.
            (*header).deallocate
        };
        unsafe {
            // Future emptiness makes cross-CPU header destruction incapable of
            // running a !Send future destructor.
            deallocate(header);
        }
    }

    pub(super) fn next(&self, kind: super::inbox::InboxKind) -> &AtomicPtr<Self> {
        match kind {
            super::inbox::InboxKind::Ready => &self.ready_next,
        }
    }

    pub(super) fn owner_next(&self) -> *mut Self {
        self.owner_next.get()
    }

    pub(super) fn set_owner_next(&self, next: *mut Self) {
        self.owner_next.set(next);
    }
}

// SAFETY: External CPUs reach only immutable metadata and atomic fields. The
// owner-only list and !Send future remain inaccessible through the public header.
unsafe impl Send for CoroutineHeader {}
// SAFETY: Shared references expose only immutable metadata and atomic operations;
// polling, owner-list mutation, and future destruction are private owner actions.
unsafe impl Sync for CoroutineHeader {}

#[repr(C)]
pub(super) struct Coroutine<F> {
    header: CoroutineHeader,
    future: UnsafeCell<Option<F>>,
}

impl<F> Coroutine<F>
where
    F: Future<Output = ()>,
{
    pub(super) fn new(id: CoroutineId, executor: Arc<SharedExecutor>, future: F) -> Self {
        Self {
            header: CoroutineHeader {
                reclaim: DeferredReclaimNode::new(reclaim_coroutine),
                id,
                state: AtomicUsize::new(0),
                references: AtomicUsize::new(1),
                executor,
                ready_next: AtomicPtr::new(ptr::null_mut()),
                owner_next: Cell::new(ptr::null_mut()),
                poll_future: poll_future::<F>,
                drop_future: drop_future::<F>,
                deallocate: deallocate::<F>,
                _pin: PhantomPinned,
            },
            future: UnsafeCell::new(Some(future)),
        }
    }
}

/// Coalesces and publishes one ready notification.
///
/// # Safety
///
/// `header` must point to a pinned coroutine allocation for which the caller owns
/// a live reference until this function returns.
pub(super) unsafe fn schedule(header: *mut CoroutineHeader) {
    let header_ref = unsafe {
        // Every caller owns a live reference for the duration of publication.
        &*header
    };
    let mut observed = header_ref.state.load(Ordering::Acquire);

    loop {
        if observed & (COMPLETE | RUN_QUEUED) != 0 {
            return;
        }
        match header_ref.state.compare_exchange_weak(
            observed,
            observed | RUN_QUEUED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(updated) => observed = updated,
        }
    }

    retain_reference(header_ref);
    if !header_ref.executor.publish_ready(header) {
        header_ref.state.fetch_and(!RUN_QUEUED, Ordering::AcqRel);
        unsafe {
            // Closing rejected this publication, so the retained queue reference
            // is released without touching the destroyed local owner object.
            release_reference(header);
        }
    }
}

pub(super) fn retain_reference(header: &CoroutineHeader) {
    let mut references = header.references.load(Ordering::Relaxed);
    loop {
        let Some(next) = references.checked_add(1) else {
            task_runtime::fatal_invariant(
                REFCOUNT_OVERFLOW_INVARIANT,
                header.id.generation() as usize,
            );
        };
        if references == 0 {
            task_runtime::fatal_invariant(
                REFCOUNT_OVERFLOW_INVARIANT,
                header.id.generation() as usize,
            );
        }
        match header.references.compare_exchange_weak(
            references,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return,
            Err(updated) => references = updated,
        }
    }
}

/// Releases one allocation reference without freeing in the calling context.
///
/// # Safety
///
/// `header` must point to a pinned coroutine allocation and the caller must own
/// exactly one reference that has not already been released.
pub(super) unsafe fn release_reference(header: *mut CoroutineHeader) {
    let header_ref = unsafe {
        // Caller relinquishes one live reference and does not use it afterward
        // unless another independently owned reference remains.
        &*header
    };
    let previous = header_ref.references.fetch_sub(1, Ordering::Release);
    if previous == 0 {
        task_runtime::fatal_invariant(
            REFCOUNT_OVERFLOW_INVARIANT,
            header_ref.id.generation() as usize,
        );
    }
    if previous != 1 {
        return;
    }
    core::sync::atomic::fence(Ordering::Acquire);

    let reclaim = unsafe {
        // The zero-reference allocation remains pinned until the task-system
        // reaper invokes its fixed callback.
        Pin::new_unchecked(&header_ref.reclaim)
    };
    crate::facade::publish_deferred_reclaim(reclaim, header.expose_provenance());
}

/// Polls the concrete future behind a type-erased header.
///
/// # Safety
///
/// `header` must denote a pinned `Coroutine<F>`, and the owner must provide
/// exclusive access to its populated future slot for the duration of the call.
unsafe fn poll_future<F>(header: *mut CoroutineHeader, context: &mut Context<'_>) -> Poll<()>
where
    F: Future<Output = ()>,
{
    let coroutine = header.cast::<Coroutine<F>>();
    let future = unsafe {
        // repr(C) places the header first. Only the UnsafeCell payload is mutably
        // borrowed; concurrent raw-waker header access remains disjoint.
        &mut *(*coroutine).future.get()
    };
    match future.as_mut() {
        Some(future) => unsafe {
            // The allocation never moves after publication.
            Pin::new_unchecked(future).poll(context)
        },
        None => Poll::Ready(()),
    }
}

/// Empties the concrete future slot behind a type-erased header.
///
/// # Safety
///
/// `header` must denote a pinned `Coroutine<F>`. The owner must call this once
/// after completion or cancellation with no overlapping poll.
unsafe fn drop_future<F>(header: *mut CoroutineHeader)
where
    F: Future<Output = ()>,
{
    let coroutine = header.cast::<Coroutine<F>>();
    let future = unsafe {
        // Owner-only completion serializes access to the UnsafeCell payload.
        &mut *(*coroutine).future.get()
    };
    let future = future.take();
    unsafe {
        // The slot is empty before user destructor code runs. An owner or queue
        // reference prevents reclamation until that destructor returns or fully
        // unwinds, while this Release publishes emptiness to the later reaper.
        (*header).state.fetch_or(FUTURE_EMPTY, Ordering::Release);
    }
    drop(future);
}

/// Reconstructs and frees the concrete coroutine allocation.
///
/// # Safety
///
/// `header` must be the first field of the original `Box<Coroutine<F>>`, have a
/// zero reference count, and contain an empty future slot.
unsafe fn deallocate<F>(header: *mut CoroutineHeader)
where
    F: Future<Output = ()>,
{
    unsafe {
        // The owner already dropped F and published FUTURE_EMPTY. Reconstructing
        // `Coroutine<F>` here could form a typed object after F's borrowing
        // lifetime ended, so the reaper touches only the non-generic header field
        // that requires destruction and then releases the raw allocation.
        core::ptr::drop_in_place(core::ptr::addr_of_mut!((*header).executor));
        dealloc(header.cast::<u8>(), Layout::new::<Coroutine<F>>());
    }
}

/// Dispatches one task-system reclaim node to its containing coroutine header.
///
/// # Safety
///
/// `node` must be the first field of a zero-reference `CoroutineHeader` detached
/// from the task-system deferred-reclaim inbox.
unsafe fn reclaim_coroutine(_node: *mut DeferredReclaimNode, data: *mut ()) {
    unsafe {
        // Publication exposes the original header pointer, preserving permission
        // for the complete allocation instead of only its first reclaim field.
        CoroutineHeader::deallocate_raw(data.cast::<CoroutineHeader>());
    }
}

#[cfg(test)]
pub(super) unsafe fn force_reference_count(header: *mut CoroutineHeader, references: usize) {
    unsafe {
        // Unit tests retain the permanent owner reference and serialize mutation.
        (*header).references.store(references, Ordering::Relaxed);
    }
}
