//! Stage-1 TLB invalidation and deferred frame ownership.

use alloc::vec::Vec;

use ax_hal::{cache::TlbShootdownError, paging::DeferredPageTableFrames};
use ax_memory_addr::{PhysAddr, VirtAddr, VirtAddrRange};

/// One page-table mutation whose resources cannot be reclaimed before every
/// potentially active CPU confirms invalidation.
#[doc(hidden)]
#[derive(Debug)]
#[must_use = "TLB gathers must be confirmed or transferred to quarantine"]
pub struct TlbGather {
    ranges: Vec<VirtAddrRange>,
    deferred_frames: Vec<PhysAddr>,
    deferred_page_tables: Vec<DeferredPageTableFrames>,
    completed: bool,
}

impl TlbGather {
    pub(crate) const fn new() -> Self {
        Self {
            ranges: Vec::new(),
            deferred_frames: Vec::new(),
            deferred_page_tables: Vec::new(),
            completed: false,
        }
    }

    pub(crate) fn invalidate(&mut self, start: VirtAddr, size: usize) {
        if size != 0 {
            self.ranges
                .push(VirtAddrRange::from_start_size(start, size));
        }
    }

    pub(crate) fn defer_frame(&mut self, frame: PhysAddr) {
        self.deferred_frames.push(frame);
    }

    pub(crate) fn defer_page_tables(&mut self, tables: DeferredPageTableFrames) {
        if !tables.is_empty() {
            self.deferred_page_tables.push(tables);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ranges.is_empty()
            && self.deferred_frames.is_empty()
            && self.deferred_page_tables.is_empty()
    }

    fn finish(self) -> Result<(), (Self, TlbShootdownError)> {
        complete_tlb_gather_with(
            self,
            ax_hal::cache::flush_tlb_range_all_cpus,
            super::backend::dealloc_frame,
        )
    }
}

impl Drop for TlbGather {
    fn drop(&mut self) {
        if !self.completed && !self.is_empty() {
            error!(
                "dropping unconfirmed stage-1 TLB gather: ranges={:?}, deferred_frames={}, \
                 deferred_page_tables={}",
                self.ranges,
                self.deferred_frames.len(),
                self.deferred_page_tables.len(),
            );
        }
    }
}

/// Address-space-owned retry queue for failed synchronous shootdowns.
pub(crate) struct TlbQuarantine {
    pending: Vec<TlbGather>,
    failures: u64,
    last_error: Option<TlbShootdownError>,
}

impl TlbQuarantine {
    pub(crate) const fn new() -> Self {
        Self {
            pending: Vec::new(),
            failures: 0,
            last_error: None,
        }
    }

    pub(crate) fn commit(&mut self, gather: TlbGather) -> Result<(), TlbShootdownError> {
        match gather.finish() {
            Ok(()) => Ok(()),
            Err((gather, error)) => {
                self.failures = self.failures.saturating_add(1);
                self.last_error = Some(error);
                self.pending.push(gather);
                Err(error)
            }
        }
    }

    pub(crate) fn retry(&mut self) -> Result<(), TlbShootdownError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let pending = core::mem::take(&mut self.pending);
        let mut iter = pending.into_iter();
        while let Some(gather) = iter.next() {
            if let Err((gather, error)) = gather.finish() {
                self.failures = self.failures.saturating_add(1);
                self.last_error = Some(error);
                self.pending.push(gather);
                self.pending.extend(iter);
                return Err(error);
            }
        }
        self.last_error = None;
        Ok(())
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub(crate) const fn failures(&self) -> u64 {
        self.failures
    }

    pub(crate) const fn last_error(&self) -> Option<TlbShootdownError> {
        self.last_error
    }
}

fn complete_tlb_gather_with<E>(
    mut gather: TlbGather,
    mut shootdown: impl FnMut(VirtAddr, usize) -> Result<(), E>,
    mut reclaim: impl FnMut(PhysAddr),
) -> Result<(), (TlbGather, E)> {
    for range in &gather.ranges {
        if let Err(error) = shootdown(range.start, range.size()) {
            return Err((gather, error));
        }
    }
    for tables in gather.deferred_page_tables.drain(..) {
        // SAFETY: every requested range has completed synchronous local and
        // remote invalidation, so no hardware walker can retain the detached
        // hierarchy.
        unsafe { tables.reclaim() };
    }
    for frame in gather.deferred_frames.drain(..) {
        reclaim(frame);
    }
    gather.completed = true;
    Ok(())
}

pub(crate) fn resolve_published_mutation<R>(
    operation_result: crate::MmResult<R>,
    shootdown_result: crate::MmResult,
) -> crate::MmResult<R> {
    if let Err(error) = shootdown_result {
        error!("published stage-1 mutation awaits quarantined TLB confirmation: {error}");
    }
    operation_result
}

pub(crate) fn resolve_confirmed_mutation<R>(
    operation_result: crate::MmResult<R>,
    shootdown_result: crate::MmResult,
) -> crate::MmResult<R> {
    shootdown_result.and(operation_result)
}

#[cfg(test)]
mod tests {
    use alloc::{rc::Rc, vec, vec::Vec};
    use core::cell::RefCell;

    use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, VirtAddr};

    use super::*;

    #[test]
    fn deferred_frames_are_reclaimed_only_after_shootdown_confirmation() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut gather = TlbGather::new();
        gather.invalidate(VirtAddr::from_usize(0x4000), PAGE_SIZE_4K);
        gather.defer_frame(PhysAddr::from_usize(0x8000));

        let shootdown_events = Rc::clone(&events);
        let reclaim_events = Rc::clone(&events);
        complete_tlb_gather_with(
            gather,
            move |_, _| {
                shootdown_events.borrow_mut().push("shootdown");
                Ok::<_, ()>(())
            },
            move |_| reclaim_events.borrow_mut().push("reclaim"),
        )
        .unwrap();

        assert_eq!(*events.borrow(), vec!["shootdown", "reclaim"]);
    }

    #[test]
    fn failed_shootdown_retains_frames_for_a_later_retry() {
        let reclaimed = Rc::new(RefCell::new(Vec::new()));
        let mut gather = TlbGather::new();
        gather.invalidate(VirtAddr::from_usize(0x4000), PAGE_SIZE_4K);
        gather.defer_frame(PhysAddr::from_usize(0x8000));

        let first_reclaim = Rc::clone(&reclaimed);
        let (_, retained) = complete_tlb_gather_with(
            gather,
            |_, _| Err("timeout"),
            move |frame| first_reclaim.borrow_mut().push(frame),
        )
        .map_err(|(gather, error)| (error, gather))
        .unwrap_err();
        assert!(reclaimed.borrow().is_empty());

        let retry_reclaim = Rc::clone(&reclaimed);
        complete_tlb_gather_with(
            retained,
            |_, _| Ok::<_, &str>(()),
            move |frame| retry_reclaim.borrow_mut().push(frame),
        )
        .unwrap();
        assert_eq!(*reclaimed.borrow(), vec![PhysAddr::from_usize(0x8000)]);
    }

    #[test]
    fn failed_confirmation_does_not_reclassify_a_published_mutation() {
        let result = resolve_published_mutation(
            Ok(17usize),
            Err(crate::MmError::TlbShootdown(TlbShootdownError::Timeout)),
        );

        assert_eq!(result, Ok(17));
    }

    #[test]
    fn confirmed_mutation_reports_failed_confirmation() {
        let result = resolve_confirmed_mutation(
            Ok(17usize),
            Err(crate::MmError::TlbShootdown(TlbShootdownError::Timeout)),
        );

        assert_eq!(
            result,
            Err(crate::MmError::TlbShootdown(TlbShootdownError::Timeout))
        );
    }
}
