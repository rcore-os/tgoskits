//! Address-space TLB shootdown and deferred reclaim transactions.

use alloc::vec::Vec;

use ax_memory_addr::{VirtAddr, VirtAddrRange};

use super::{Backend, backend::DeferredFrameRelease};

/// Collects page-table invalidations and ownership that cannot be released
/// before every active CPU has acknowledged the invalidation.
pub struct TlbGather {
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

    pub(super) fn retain_backend(&mut self, backend: Backend) {
        self.retained_backends.push(backend);
    }

    pub(super) fn defer_frame(&mut self, frame: DeferredFrameRelease) {
        self.deferred_frames.push(frame);
    }

    pub(super) fn finish(mut self, cpu_mask: usize) -> crate::StarryResult {
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
pub(super) fn checked_range(start: VirtAddr, size: usize) -> crate::StarryResult<VirtAddrRange> {
    VirtAddrRange::try_from_start_size(start, size).ok_or(crate::StarryError::InvalidInput)
}
