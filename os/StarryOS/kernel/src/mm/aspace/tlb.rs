//! Address-space TLB shootdown and deferred reclaim transactions.

use alloc::vec::Vec;
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{VirtAddr, VirtAddrRange};
use scope_local::scope_local;

use super::{Backend, backend::DeferredFrameRelease};

scope_local! {
    static ACTIVE_TLB_GATHER: AtomicUsize = AtomicUsize::new(0);
}

/// Collects page-table invalidations and ownership that cannot be released
/// before every active CPU has acknowledged the invalidation.
pub(super) struct TlbGather {
    ranges: InlineStorage<VirtAddrRange>,
    retained_backends: InlineStorage<Backend>,
    deferred_frames: InlineStorage<DeferredFrameRelease>,
}

/// Keeps the common one-item gather path allocation-free.
struct InlineStorage<T> {
    first: Option<T>,
    overflow: Vec<T>,
}

impl<T> InlineStorage<T> {
    const fn new() -> Self {
        Self {
            first: None,
            overflow: Vec::new(),
        }
    }

    fn push(&mut self, value: T) {
        if self.first.is_none() {
            self.first = Some(value);
        } else {
            self.overflow.push(value);
        }
    }

    fn last_mut(&mut self) -> Option<&mut T> {
        self.overflow.last_mut().or(self.first.as_mut())
    }

    fn iter(&self) -> impl Iterator<Item = &T> {
        self.first.iter().chain(self.overflow.iter())
    }
}

impl TlbGather {
    pub(super) const fn new() -> Self {
        Self {
            ranges: InlineStorage::new(),
            retained_backends: InlineStorage::new(),
            deferred_frames: InlineStorage::new(),
        }
    }

    pub(super) fn record_range(&mut self, range: VirtAddrRange) {
        if range.is_empty() {
            return;
        }
        if let Some(last) = self.ranges.last_mut()
            && range.start <= last.end
            && last.start <= range.end
        {
            last.start = last.start.min(range.start);
            last.end = last.end.max(range.end);
        } else {
            self.ranges.push(range);
        }
    }

    fn retain_backend(&mut self, backend: Backend) {
        self.retained_backends.push(backend);
    }

    fn defer_frame(&mut self, frame: DeferredFrameRelease) {
        self.deferred_frames.push(frame);
    }

    pub(super) fn finish(mut self, cpu_mask: usize) -> AxResult {
        for range in self.ranges.iter() {
            if let Err(error) =
                crate::mm::flush_tlb_range_on_cpus_sync(cpu_mask, range.start, range.size())
            {
                // Continuing after an incomplete shootdown would turn a
                // recoverable resource leak into a cross-CPU use-after-free.
                core::mem::forget(self.deferred_frames);
                core::mem::forget(self.retained_backends);
                return Err(error);
            }
        }
        if let Some(frame) = self.deferred_frames.first.take() {
            frame.release();
        }
        for frame in self.deferred_frames.overflow.drain(..) {
            frame.release();
        }
        self.retained_backends.first.take();
        self.retained_backends.overflow.clear();
        Ok(())
    }
}

/// Validates a range before any page-table mutation begins.
pub(super) fn checked_range(start: VirtAddr, size: usize) -> AxResult<VirtAddrRange> {
    VirtAddrRange::try_from_start_size(start, size).ok_or(AxError::InvalidInput)
}

/// Publishes one gather to backend calls made through `ax-memory-set`.
pub(super) struct TlbGatherGuard<'a> {
    previous: usize,
    _borrow: PhantomData<&'a mut TlbGather>,
}

impl<'a> TlbGatherGuard<'a> {
    pub(super) fn enter(gather: &'a mut TlbGather) -> Self {
        let pointer = gather as *mut TlbGather as usize;
        let previous = ACTIVE_TLB_GATHER.with(|slot| slot.swap(pointer, Ordering::Relaxed));
        Self {
            previous,
            _borrow: PhantomData,
        }
    }
}

impl Drop for TlbGatherGuard<'_> {
    fn drop(&mut self) {
        ACTIVE_TLB_GATHER.with(|slot| slot.store(self.previous, Ordering::Relaxed));
    }
}

fn with_active_gather<R>(operation: impl FnOnce(&mut TlbGather) -> R) -> Option<R> {
    let pointer = ACTIVE_TLB_GATHER.with(|slot| slot.load(Ordering::Relaxed));
    if pointer == 0 {
        return None;
    }
    // SAFETY: `TlbGatherGuard` keeps the pointed-to gather alive and backend
    // bridge calls are serialized by the current task's address-space
    // transaction. The pointer is restored before the gather is consumed.
    Some(operation(unsafe { &mut *(pointer as *mut TlbGather) }))
}

pub(super) fn retain_backend_until_shootdown(backend: Backend) {
    with_active_gather(|gather| gather.retain_backend(backend))
        .unwrap_or_else(|| panic!("backend unmap requires an active TLB gather"));
}

pub(super) fn record_range_for_shootdown(range: VirtAddrRange) {
    with_active_gather(|gather| gather.record_range(range))
        .unwrap_or_else(|| panic!("page-table mutation requires an active TLB gather"));
}

pub(super) fn defer_frame_until_shootdown(frame: DeferredFrameRelease) {
    let mut frame = Some(frame);
    let deferred = with_active_gather(|gather| {
        gather.defer_frame(frame.take().expect("deferred frame consumed once"));
    });
    if deferred.is_none() {
        core::mem::forget(frame.take());
        panic!("frame release requires an active TLB gather");
    }
}
