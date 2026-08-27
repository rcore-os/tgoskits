//! Address-space TLB shootdown and deferred reclaim transactions.

use alloc::vec::Vec;

use ax_fs_ng::{file::PageCache, vfs::CachedFile};
use ax_memory_addr::{VirtAddr, VirtAddrRange};
use ax_runtime::hal::cache::TlbShootdownError;

use super::{Backend, backend::DeferredFrameRelease};

/// Collects page-table invalidations and ownership that cannot be released
/// before every active CPU has acknowledged the invalidation.
#[must_use = "TLB gathers must be confirmed or transferred to quarantine"]
pub struct TlbGather {
    ranges: InlineStorage<VirtAddrRange>,
    deferred: Option<DeferredResources>,
    completed: bool,
}

struct DeferredResources {
    retained_backends: InlineStorage<Backend>,
    deferred_frames: InlineStorage<DeferredFrameRelease>,
    retained_file_pages: Vec<RetainedFilePage>,
}

pub(super) struct RetainedFilePage {
    pub(super) page_number: u32,
    pub(super) cache: CachedFile,
    _page: PageCache,
}

impl DeferredResources {
    const fn new() -> Self {
        Self {
            retained_backends: InlineStorage::new(),
            deferred_frames: InlineStorage::new(),
            retained_file_pages: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.retained_backends.is_empty()
            && self.deferred_frames.is_empty()
            && self.retained_file_pages.is_empty()
    }

    fn release(mut self) {
        if let Some(frame) = self.deferred_frames.first.take() {
            frame.release();
        }
        for frame in self.deferred_frames.overflow.drain(..) {
            frame.release();
        }
        // Backends and file-cache pages release their owned resources on drop,
        // after every selected CPU has acknowledged the invalidation.
    }
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

    fn try_reserve(
        &mut self,
        additional: usize,
    ) -> Result<(), alloc::collections::TryReserveError> {
        let inline_capacity = usize::from(self.first.is_none());
        self.overflow
            .try_reserve(additional.saturating_sub(inline_capacity))
    }

    fn has_spare_capacity(&self) -> bool {
        self.first.is_none() || self.overflow.len() < self.overflow.capacity()
    }

    fn push_reserved(&mut self, value: T) {
        if self.first.is_none() {
            self.first = Some(value);
        } else {
            assert!(
                self.overflow.len() < self.overflow.capacity(),
                "TLB gather ownership must be reserved before PTE mutation"
            );
            self.overflow.push(value);
        }
    }

    fn last_mut(&mut self) -> Option<&mut T> {
        self.overflow.last_mut().or(self.first.as_mut())
    }

    fn iter(&self) -> impl Iterator<Item = &T> {
        self.first.iter().chain(self.overflow.iter())
    }

    fn is_empty(&self) -> bool {
        self.first.is_none() && self.overflow.is_empty()
    }

    fn clear(&mut self) {
        self.first = None;
        self.overflow.clear();
    }
}

impl TlbGather {
    pub(super) const fn new() -> Self {
        Self {
            ranges: InlineStorage::new(),
            deferred: Some(DeferredResources::new()),
            completed: false,
        }
    }

    fn deferred_mut(&mut self) -> &mut DeferredResources {
        self.deferred
            .as_mut()
            .expect("an unfinished TLB gather must own deferred resources")
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
        } else if self.ranges.has_spare_capacity() {
            self.ranges.push_reserved(range);
        } else {
            // Recording follows the PTE write and therefore cannot allocate or
            // fail. If the prepared disjoint-range capacity is exhausted,
            // conservatively collapse to one larger invalidation range.
            let start = self
                .ranges
                .iter()
                .fold(range.start, |start, existing| start.min(existing.start));
            let end = self
                .ranges
                .iter()
                .fold(range.end, |end, existing| end.max(existing.end));
            self.ranges.clear();
            self.ranges
                .push_reserved(VirtAddrRange::new(start, end));
        }
    }

    pub(super) fn prepare_ranges(
        &mut self,
        count: usize,
    ) -> Result<(), alloc::collections::TryReserveError> {
        self.ranges.try_reserve(count)
    }

    pub(super) fn prepare_backend_retention(
        &mut self,
        count: usize,
    ) -> Result<(), alloc::collections::TryReserveError> {
        self.deferred_mut().retained_backends.try_reserve(count)
    }

    pub(super) fn retain_backend(&mut self, backend: Backend) {
        self.deferred_mut()
            .retained_backends
            .push_reserved(backend);
    }

    pub(super) fn prepare_deferred_frames(
        &mut self,
        count: usize,
    ) -> Result<(), alloc::collections::TryReserveError> {
        self.deferred_mut().deferred_frames.try_reserve(count)
    }

    pub(super) fn defer_frame(&mut self, frame: DeferredFrameRelease) {
        self.deferred_mut().deferred_frames.push_reserved(frame);
    }

    /// Transfers one evicted page-cache frame into this transaction before any
    /// later fallible page-table work can return to the caller.
    pub(super) fn retain_file_page(
        &mut self,
        page_number: u32,
        cache: CachedFile,
        page: PageCache,
    ) {
        self.deferred_mut()
            .retained_file_pages
            .push(RetainedFilePage {
                page_number,
                cache,
                _page: page,
            });
    }

    /// Reserves ownership storage before page-cache eviction can publish any
    /// listener invalidation or detach the old frame.
    pub(super) fn prepare_file_page_retention(
        &mut self,
    ) -> Result<(), alloc::collections::TryReserveError> {
        self.deferred_mut().retained_file_pages.try_reserve(1)
    }

    pub(super) fn take_retained_file_evictions(&mut self) -> Vec<RetainedFilePage> {
        core::mem::take(&mut self.deferred_mut().retained_file_pages)
    }

    pub(super) fn restore_retained_file_evictions(&mut self, retained: Vec<RetainedFilePage>) {
        assert!(self.deferred_mut().retained_file_pages.is_empty());
        self.deferred_mut().retained_file_pages = retained;
    }

    fn is_empty(&self) -> bool {
        self.ranges.is_empty()
            && self
                .deferred
                .as_ref()
                .is_none_or(DeferredResources::is_empty)
    }

    fn finish(&mut self, cpu_mask: usize) -> Result<(), TlbShootdownError> {
        for range in self.ranges.iter() {
            ax_runtime::hal::cache::flush_tlb_range_on_cpus(
                cpu_mask,
                range.start,
                range.size(),
            )?;
        }
        self.deferred
            .take()
            .expect("an unfinished TLB gather must own deferred resources")
            .release();
        self.completed = true;
        Ok(())
    }
}

impl Drop for TlbGather {
    fn drop(&mut self) {
        if !self.completed && !self.is_empty() {
            let deferred = self
                .deferred
                .as_ref()
                .expect("an unfinished TLB gather must own deferred resources");
            error!(
                "dropping unconfirmed Starry TLB gather: ranges={}, backends={}, \
                 frames={}, file_pages={}",
                self.ranges.iter().count(),
                deferred.retained_backends.iter().count(),
                deferred.deferred_frames.iter().count(),
                deferred.retained_file_pages.len(),
            );
        }
    }
}

/// Address-space-owned retry queue for failed synchronous shootdowns.
pub(super) struct TlbQuarantine {
    // A new mutation cannot begin until the previous gather is confirmed, so
    // at most one failed transaction can be pending. Keeping it inline makes
    // the failure handoff allocation-free.
    pending: Option<TlbGather>,
    failures: u64,
    last_error: Option<TlbShootdownError>,
}

impl TlbQuarantine {
    pub(super) const fn new() -> Self {
        Self {
            pending: None,
            failures: 0,
            last_error: None,
        }
    }

    pub(super) fn commit(
        &mut self,
        mut gather: TlbGather,
        cpu_mask: usize,
    ) -> Result<(), TlbShootdownError> {
        match gather.finish(cpu_mask) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                self.last_error = Some(error);
                assert!(
                    self.pending.is_none(),
                    "a new TLB gather cannot bypass an existing quarantine"
                );
                self.pending = Some(gather);
                Err(error)
            }
        }
    }

    pub(super) fn retry(&mut self, cpu_mask: usize) -> Result<(), TlbShootdownError> {
        let Some(mut gather) = self.pending.take() else {
            return Ok(());
        };
        if let Err(error) = gather.finish(cpu_mask) {
            self.failures = self.failures.saturating_add(1);
            self.last_error = Some(error);
            self.pending = Some(gather);
            return Err(error);
        }
        self.last_error = None;
        Ok(())
    }

    pub(super) fn pending_count(&self) -> usize {
        usize::from(self.pending.is_some())
    }

    pub(super) const fn failures(&self) -> u64 {
        self.failures
    }

    pub(super) const fn last_error(&self) -> Option<TlbShootdownError> {
        self.last_error
    }
}

#[cfg(all(test, not(axtest)))]
fn complete_deferred_resources_with<T, E>(
    resources: T,
    confirm: impl FnOnce() -> Result<(), E>,
    release: impl FnOnce(T),
) -> Result<(), (T, E)> {
    if let Err(error) = confirm() {
        return Err((resources, error));
    }
    release(resources);
    Ok(())
}

/// Validates a range before any page-table mutation begins.
pub(super) fn checked_range(start: VirtAddr, size: usize) -> crate::StarryResult<VirtAddrRange> {
    VirtAddrRange::try_from_start_size(start, size).ok_or(crate::StarryError::InvalidInput)
}

pub(super) fn resolve_published_mutation<R>(
    operation_result: crate::StarryResult<R>,
    shootdown_result: crate::StarryResult,
) -> crate::StarryResult<R> {
    if let Err(error) = shootdown_result {
        error!(
            "published address-space mutation awaits quarantined TLB confirmation: {error}"
        );
    }
    operation_result
}

/// The two independently meaningful results of a published page-table
/// mutation.
///
/// Most syscall callers must report the operation result once the PTE change
/// is visible, even when remote confirmation is quarantined. Resource-release
/// callbacks instead consume [`Self::into_confirmed_result`] so they retain the
/// external owner until every stale translation is invalidated.
pub(super) struct PublishedMutation<R> {
    operation_result: crate::StarryResult<R>,
    shootdown_result: crate::StarryResult,
}

impl<R> PublishedMutation<R> {
    pub(super) const fn new(
        operation_result: crate::StarryResult<R>,
        shootdown_result: crate::StarryResult,
    ) -> Self {
        Self {
            operation_result,
            shootdown_result,
        }
    }

    pub(super) fn into_operation_result(self) -> crate::StarryResult<R> {
        resolve_published_mutation(self.operation_result, self.shootdown_result)
    }

    pub(super) fn into_confirmed_result(self) -> crate::StarryResult<R> {
        match self.operation_result {
            Err(operation_error) => {
                if let Err(shootdown_error) = self.shootdown_result {
                    error!(
                        "failed address-space mutation also awaits quarantined TLB \
                         confirmation: {shootdown_error}"
                    );
                }
                Err(operation_error)
            }
            Ok(value) => self.shootdown_result.map(|()| value),
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use alloc::{rc::Rc, vec};
    use core::cell::Cell;

    use super::*;

    #[derive(Debug)]
    struct DropProbe(Rc<Cell<usize>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn failed_shootdown_returns_deferred_resources_for_retry() {
        let drops = Rc::new(Cell::new(0));
        let resources = vec![DropProbe(Rc::clone(&drops))];

        let (resources, error) =
            complete_deferred_resources_with(resources, || Err("timeout"), drop)
                .expect_err("failed shootdown must retain ownership for retry");
        assert_eq!(error, "timeout");
        assert_eq!(drops.get(), 0, "failure must not release a retained page");

        complete_deferred_resources_with(resources, || Ok::<_, &str>(()), drop)
            .expect("successful retry must release the retained page");
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn failed_confirmation_does_not_reclassify_a_published_mutation() {
        let result = resolve_published_mutation(
            Ok(17usize),
            Err(crate::StarryError::TlbShootdown(
                TlbShootdownError::Timeout,
            )),
        );

        assert!(matches!(result, Ok(17)));
    }

    #[test]
    fn resource_release_observes_failed_confirmation() {
        let result = PublishedMutation::new(
            Ok(17usize),
            Err(crate::StarryError::TlbShootdown(
                TlbShootdownError::Timeout,
            )),
        )
        .into_confirmed_result();

        assert!(matches!(
            result,
            Err(crate::StarryError::TlbShootdown(
                TlbShootdownError::Timeout
            ))
        ));
    }

    #[test]
    fn exhausted_range_storage_collapses_without_losing_coverage() {
        let mut gather = TlbGather::new();
        gather.record_range(VirtAddrRange::from_start_size(
            VirtAddr::from(0x1000),
            0x1000,
        ));
        gather.record_range(VirtAddrRange::from_start_size(
            VirtAddr::from(0x9000),
            0x1000,
        ));

        let ranges = gather.ranges.iter().copied().collect::<Vec<_>>();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, VirtAddr::from(0x1000));
        assert_eq!(ranges[0].end, VirtAddr::from(0xa000));
        gather.completed = true;
    }
}
