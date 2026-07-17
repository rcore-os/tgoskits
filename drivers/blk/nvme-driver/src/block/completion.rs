//! Queue-local completion cache populated only by acknowledged IRQ service.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

use crate::queue::{NvmeCompletion, NvmeQueue as HardwareQueue};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CachedCompletion {
    pub(super) cid: usize,
    pub(super) status: CompletionStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CompletionStatus {
    pub(super) success: bool,
    pub(super) raw_status: u16,
    pub(super) result: u64,
}

pub(super) struct CompletionCache {
    entries: Vec<CompletionCacheEntry>,
}

pub(super) struct IrqCompletionContinuation {
    pending: AtomicBool,
}

#[derive(Clone, Copy)]
pub(super) struct ReadyCompletionSnapshot(u64);

struct CompletionCacheEntry {
    ready: AtomicBool,
    success: AtomicBool,
    raw_status: AtomicU16,
    result: AtomicU64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CompletionDrain {
    pub(super) completed: usize,
    pub(super) continuation: bool,
    pub(super) invalid: bool,
}

impl CompletionDrain {
    pub(super) const fn deferred() -> Self {
        Self {
            completed: 0,
            continuation: true,
            invalid: false,
        }
    }

    pub(super) const fn needs_service(self) -> bool {
        self.completed != 0 || self.continuation || self.invalid
    }
}

pub(super) fn drain_hardware_completions_to_cache(
    queue: &HardwareQueue,
    cache: &CompletionCache,
    budget: usize,
) -> CompletionDrain {
    drain_completion_source(
        || queue.take_irq_completion().map(CachedCompletion::from),
        cache,
        budget,
    )
}

pub(super) fn drain_completion_source(
    mut next: impl FnMut() -> Option<CachedCompletion>,
    cache: &CompletionCache,
    budget: usize,
) -> CompletionDrain {
    if budget == 0 {
        return CompletionDrain::deferred();
    }
    let mut completed = 0;
    let mut invalid = false;
    while completed < budget {
        let Some(completion) = next() else {
            return CompletionDrain {
                completed,
                continuation: false,
                invalid,
            };
        };
        invalid |= !cache.record(completion);
        completed += 1;
    }
    CompletionDrain {
        completed,
        continuation: true,
        invalid,
    }
}

impl CompletionCache {
    pub(super) fn new(capacity: usize) -> Self {
        assert!(
            capacity <= ReadyCompletionSnapshot::MAX_CID + 1,
            "NVMe completion cache exceeds the bounded CID snapshot"
        );
        let mut entries = Vec::with_capacity(capacity);
        entries.resize_with(capacity, CompletionCacheEntry::new);
        Self { entries }
    }

    pub(super) fn record(&self, completion: CachedCompletion) -> bool {
        // CID zero is reserved by this queue, and a second unconsumed CQE for
        // one CID would otherwise overwrite the first request result.
        if completion.cid == 0 {
            return false;
        }
        let Some(entry) = self.entries.get(completion.cid) else {
            return false;
        };
        if entry.ready.load(Ordering::Acquire) {
            return false;
        }
        entry
            .success
            .store(completion.status.success, Ordering::Relaxed);
        entry
            .raw_status
            .store(completion.status.raw_status, Ordering::Relaxed);
        entry
            .result
            .store(completion.status.result, Ordering::Relaxed);
        entry.ready.store(true, Ordering::Release);
        true
    }

    pub(super) fn take(&self, cid: usize) -> Option<CompletionStatus> {
        self.take_with_interleave(cid, || {})
    }

    fn take_with_interleave(
        &self,
        cid: usize,
        before_ready_release: impl FnOnce(),
    ) -> Option<CompletionStatus> {
        let entry = self.entries.get(cid)?;
        if !entry.ready.load(Ordering::Acquire) {
            return None;
        }
        let status = CompletionStatus {
            success: entry.success.load(Ordering::Relaxed),
            raw_status: entry.raw_status.load(Ordering::Relaxed),
            result: entry.result.load(Ordering::Relaxed),
        };
        // Keep `ready` published until every payload field has been copied.
        // A concurrent IRQ must classify a duplicate/late CQE as invalid,
        // rather than overwrite the result currently being consumed.
        before_ready_release();
        entry.ready.store(false, Ordering::Release);
        Some(status)
    }

    pub(super) fn has_ready(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.ready.load(Ordering::Acquire))
    }

    pub(super) fn ready_snapshot(&self) -> ReadyCompletionSnapshot {
        let mut ready = 0;
        for (cid, entry) in self.entries.iter().enumerate().skip(1) {
            if entry.ready.load(Ordering::Acquire) {
                ready |= 1_u64 << (cid - 1);
            }
        }
        ReadyCompletionSnapshot(ready)
    }

    pub(super) fn clear_after_quiesce(&self) {
        for entry in &self.entries {
            entry.ready.store(false, Ordering::Release);
            entry.success.store(false, Ordering::Relaxed);
            entry.raw_status.store(0, Ordering::Relaxed);
            entry.result.store(0, Ordering::Relaxed);
        }
    }
}

impl ReadyCompletionSnapshot {
    pub(super) const MAX_CID: usize = u64::BITS as usize;

    pub(super) const fn contains(self, cid: usize) -> bool {
        cid != 0 && cid <= Self::MAX_CID && self.0 & (1_u64 << (cid - 1)) != 0
    }
}

impl IrqCompletionContinuation {
    pub(super) const fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    pub(super) fn request(&self) {
        self.pending.store(true, Ordering::Release);
    }

    pub(super) fn take_for_service(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub(super) fn clear_after_quiesce(&self) {
        self.pending.store(false, Ordering::Release);
    }
}

impl CompletionCacheEntry {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            success: AtomicBool::new(false),
            raw_status: AtomicU16::new(0),
            result: AtomicU64::new(0),
        }
    }
}

impl From<NvmeCompletion> for CachedCompletion {
    fn from(completion: NvmeCompletion) -> Self {
        Self {
            cid: usize::from(completion.command_id),
            status: CompletionStatus {
                success: completion.status.is_success(),
                raw_status: completion.status.0,
                result: completion.result,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CachedCompletion, CompletionCache, CompletionStatus, IrqCompletionContinuation};

    #[test]
    fn consumer_keeps_cache_slot_owned_until_payload_is_copied() {
        let cache = CompletionCache::new(2);
        let first = CachedCompletion {
            cid: 1,
            status: CompletionStatus {
                success: true,
                raw_status: 0,
                result: 0x11,
            },
        };
        let late_duplicate = CachedCompletion {
            cid: 1,
            status: CompletionStatus {
                success: false,
                raw_status: 0xdead,
                result: 0x22,
            },
        };
        assert!(cache.record(first));

        let mut duplicate_was_accepted = false;
        let observed = cache
            .take_with_interleave(1, || {
                duplicate_was_accepted = cache.record(late_duplicate);
            })
            .expect("the first completion must remain consumable");

        assert!(
            !duplicate_was_accepted,
            "the cache slot must remain owned while its payload is copied"
        );
        assert_eq!(observed, first.status);
    }

    #[test]
    fn task_service_cannot_read_cq_without_irq_continuation_credit() {
        let continuation = IrqCompletionContinuation::new();

        assert!(
            !continuation.take_for_service(),
            "an empty continuation token must not become a completion poll"
        );
    }

    #[test]
    fn one_irq_continuation_credit_is_consumed_once() {
        let continuation = IrqCompletionContinuation::new();
        continuation.request();

        assert!(continuation.take_for_service());
        assert!(!continuation.take_for_service());
    }
}
