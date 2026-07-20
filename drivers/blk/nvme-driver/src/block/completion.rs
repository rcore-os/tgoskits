//! Queue-local completion cache populated only by the maintenance owner.

use alloc::vec::Vec;

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
    entries: Vec<Option<CompletionStatus>>,
}

#[derive(Clone, Copy)]
pub(super) struct ReadyCompletionSnapshot(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CompletionDrain {
    pub(super) completed: usize,
    pub(super) may_have_more: bool,
    pub(super) invalid: bool,
}

impl CompletionDrain {
    pub(super) const fn budget_exhausted() -> Self {
        Self {
            completed: 0,
            may_have_more: true,
            invalid: false,
        }
    }
}

pub(super) fn drain_owner_completions_to_cache(
    queue: &HardwareQueue,
    cache: &mut CompletionCache,
    budget: usize,
) -> CompletionDrain {
    drain_completion_source(
        || queue.take_owner_completion().map(CachedCompletion::from),
        cache,
        budget,
    )
}

pub(super) fn drain_completion_source(
    mut next: impl FnMut() -> Option<CachedCompletion>,
    cache: &mut CompletionCache,
    budget: usize,
) -> CompletionDrain {
    if budget == 0 {
        return CompletionDrain::budget_exhausted();
    }
    let mut completed = 0;
    let mut invalid = false;
    while completed < budget {
        let Some(completion) = next() else {
            return CompletionDrain {
                completed,
                may_have_more: false,
                invalid,
            };
        };
        invalid |= !cache.record(completion);
        completed += 1;
    }
    CompletionDrain {
        completed,
        may_have_more: true,
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
        entries.resize(capacity, None);
        Self { entries }
    }

    pub(super) fn record(&mut self, completion: CachedCompletion) -> bool {
        // CID zero is reserved by this queue, and a second unconsumed CQE for
        // one CID would otherwise overwrite the first request result.
        if completion.cid == 0 {
            return false;
        }
        let Some(entry) = self.entries.get(completion.cid) else {
            return false;
        };
        if entry.is_some() {
            return false;
        }
        self.entries[completion.cid] = Some(completion.status);
        true
    }

    pub(super) fn take(&mut self, cid: usize) -> Option<CompletionStatus> {
        self.entries.get_mut(cid)?.take()
    }

    pub(super) fn has_ready(&self) -> bool {
        self.entries.iter().any(Option::is_some)
    }

    pub(super) fn ready_snapshot(&self) -> ReadyCompletionSnapshot {
        let mut ready = 0;
        for (cid, entry) in self.entries.iter().enumerate().skip(1) {
            if entry.is_some() {
                ready |= 1_u64 << (cid - 1);
            }
        }
        ReadyCompletionSnapshot(ready)
    }

    pub(super) fn clear_after_quiesce(&mut self) {
        for entry in &mut self.entries {
            *entry = None;
        }
    }
}

impl ReadyCompletionSnapshot {
    pub(super) const MAX_CID: usize = u64::BITS as usize;

    pub(super) const fn contains(self, cid: usize) -> bool {
        cid != 0 && cid <= Self::MAX_CID && self.0 & (1_u64 << (cid - 1)) != 0
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
    use super::{CachedCompletion, CompletionCache, CompletionStatus, drain_completion_source};

    #[test]
    fn duplicate_cqe_cannot_replace_owner_retained_completion() {
        let mut cache = CompletionCache::new(2);
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
        assert!(!cache.record(late_duplicate));
        assert_eq!(cache.take(1), Some(first.status));
    }

    #[test]
    fn zero_budget_retains_the_acknowledged_batch_without_reading_the_cq() {
        let mut cache = CompletionCache::new(2);
        let mut reads = 0;

        let drain = drain_completion_source(
            || {
                reads += 1;
                None
            },
            &mut cache,
            0,
        );

        assert_eq!(reads, 0);
        assert_eq!(drain.completed, 0);
        assert!(drain.may_have_more);
    }
}
