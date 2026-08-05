use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Cumulative blk-mq dispatch and terminal-completion counters.
///
/// These counters describe runtime-to-driver batches, not filesystem calls.
/// A native NVMe batch can therefore contain multiple requests even when they
/// originated from one blocking filesystem operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlockBatchStats {
    pub submission_batches: u64,
    pub submitted_requests: u64,
    pub commit_calls: u64,
    pub commit_failures: u64,
    pub completed_requests: u64,
    pub failed_requests: u64,
    pub largest_batch: usize,
    pub peak_inflight: usize,
}

#[derive(Default)]
struct BlockBatchMetrics {
    submission_batches: AtomicU64,
    submitted_requests: AtomicU64,
    commit_calls: AtomicU64,
    commit_failures: AtomicU64,
    completed_requests: AtomicU64,
    failed_requests: AtomicU64,
    largest_batch: AtomicUsize,
    peak_inflight: AtomicUsize,
}

static BLOCK_BATCH_METRICS: BlockBatchMetrics = BlockBatchMetrics::new();

/// Returns one relaxed snapshot of cumulative blk-mq batch counters.
///
/// Individual fields may advance while the snapshot is read. The values are
/// intended for diagnostics and benchmark evidence, not synchronization.
pub fn block_batch_stats() -> BlockBatchStats {
    BLOCK_BATCH_METRICS.snapshot()
}

pub(super) fn record_submission_batch(accepted: usize, inflight: usize) {
    BLOCK_BATCH_METRICS.record_submission_batch(accepted, inflight);
}

pub(super) fn record_commit(result: Result<(), rdif_block::BlkError>) {
    BLOCK_BATCH_METRICS.record_commit(result.is_err());
}

pub(super) fn record_terminal_completion(failed: bool) {
    BLOCK_BATCH_METRICS.record_terminal_completion(failed);
}

impl BlockBatchMetrics {
    const fn new() -> Self {
        Self {
            submission_batches: AtomicU64::new(0),
            submitted_requests: AtomicU64::new(0),
            commit_calls: AtomicU64::new(0),
            commit_failures: AtomicU64::new(0),
            completed_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            largest_batch: AtomicUsize::new(0),
            peak_inflight: AtomicUsize::new(0),
        }
    }

    fn record_submission_batch(&self, accepted: usize, inflight: usize) {
        if accepted == 0 {
            return;
        }
        self.submission_batches.fetch_add(1, Ordering::Relaxed);
        self.submitted_requests.fetch_add(
            u64::try_from(accepted).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        self.largest_batch.fetch_max(accepted, Ordering::Relaxed);
        self.peak_inflight.fetch_max(inflight, Ordering::Relaxed);
    }

    fn record_commit(&self, failed: bool) {
        self.commit_calls.fetch_add(1, Ordering::Relaxed);
        if failed {
            self.commit_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn record_terminal_completion(&self, failed: bool) {
        self.completed_requests.fetch_add(1, Ordering::Relaxed);
        if failed {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn snapshot(&self) -> BlockBatchStats {
        BlockBatchStats {
            submission_batches: self.submission_batches.load(Ordering::Relaxed),
            submitted_requests: self.submitted_requests.load(Ordering::Relaxed),
            commit_calls: self.commit_calls.load(Ordering::Relaxed),
            commit_failures: self.commit_failures.load(Ordering::Relaxed),
            completed_requests: self.completed_requests.load(Ordering::Relaxed),
            failed_requests: self.failed_requests.load(Ordering::Relaxed),
            largest_batch: self.largest_batch.load(Ordering::Relaxed),
            peak_inflight: self.peak_inflight.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BlockBatchMetrics;

    #[test]
    fn batch_metrics_distinguish_dispatch_commit_and_completion() {
        let metrics = BlockBatchMetrics::new();

        metrics.record_submission_batch(4, 4);
        metrics.record_submission_batch(2, 5);
        metrics.record_commit(false);
        metrics.record_commit(true);
        metrics.record_terminal_completion(false);
        metrics.record_terminal_completion(true);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.submission_batches, 2);
        assert_eq!(snapshot.submitted_requests, 6);
        assert_eq!(snapshot.commit_calls, 2);
        assert_eq!(snapshot.commit_failures, 1);
        assert_eq!(snapshot.completed_requests, 2);
        assert_eq!(snapshot.failed_requests, 1);
        assert_eq!(snapshot.largest_batch, 4);
        assert_eq!(snapshot.peak_inflight, 5);
    }
}
