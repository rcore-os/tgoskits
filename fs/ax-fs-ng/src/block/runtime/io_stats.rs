//! Per-device accounting owned by request completion, independent of waiters.

use alloc::sync::Arc;
use core::time::Duration;

use rdif_block::RequestOp;

use crate::os::{monotonic_time, sync::RawSpinLock};

/// One device's cumulative admitted-request I/O statistics.
///
/// Durations use monotonic nanoseconds; sectors are terminally retired request
/// bytes in 512-byte units, including bytes completed with an I/O error. They
/// are not a measure of successful hardware transfers. Accounting begins after
/// runtime admission and includes software channel and hardware retry waits.
/// In-flight and busy time cover read/write requests. `Flush` is the separate
/// hardware flush command, like Linux's internal `flush_rq`, not its containing
/// write/PREFLUSH request. Weighted time sums completed read/write/flush latency;
/// snapshots never charge unfinished request latency to that field. A cancelled
/// pre-enqueue admission releases in-flight state without completing an I/O.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlockIoStats {
    pub reads: u64,
    pub sectors_read: u64,
    pub read_ns: u64,
    pub writes: u64,
    pub sectors_written: u64,
    pub write_ns: u64,
    pub flushes: u64,
    pub flush_ns: u64,
    pub failed_requests: u64,
    pub inflight: u64,
    pub busy_ns: u64,
    pub weighted_ns: u64,
}

#[derive(Default)]
struct IoState {
    counters: BlockIoStats,
    updated: Duration,
}

#[derive(Default)]
pub(super) struct DeviceIoMetrics {
    state: RawSpinLock<IoState>,
}

impl DeviceIoMetrics {
    pub(super) fn snapshot(&self) -> BlockIoStats {
        let mut state = self.state.lock_irqsave();
        state.advance(monotonic_time());
        state.counters
    }
}

impl IoState {
    fn advance(&mut self, now: Duration) {
        let elapsed = nanos(now.saturating_sub(self.updated));
        if self.counters.inflight != 0 {
            self.counters.busy_ns = self.counters.busy_ns.saturating_add(elapsed);
        }
        self.updated = self.updated.max(now);
    }

    fn begin(&mut self, now: Duration, op: RequestOp) {
        self.advance(now);
        if op != RequestOp::Flush {
            self.counters.inflight += 1;
        }
    }

    fn cancel(&mut self, now: Duration, op: RequestOp) {
        self.advance(now);
        if op != RequestOp::Flush {
            self.counters.inflight -= 1;
        }
    }

    fn complete(
        &mut self,
        now: Duration,
        started: Duration,
        op: RequestOp,
        sectors: u64,
        failed: bool,
    ) {
        self.advance(now);
        if op != RequestOp::Flush {
            self.counters.inflight -= 1;
        }
        let elapsed = nanos(now.saturating_sub(started));
        self.counters.weighted_ns = self.counters.weighted_ns.saturating_add(elapsed);
        if failed {
            self.counters.failed_requests += 1;
        }
        match op {
            RequestOp::Read => {
                self.counters.reads += 1;
                self.counters.sectors_read = self.counters.sectors_read.saturating_add(sectors);
                self.counters.read_ns = self.counters.read_ns.saturating_add(elapsed);
            }
            RequestOp::Write => {
                self.counters.writes += 1;
                self.counters.sectors_written =
                    self.counters.sectors_written.saturating_add(sectors);
                self.counters.write_ns = self.counters.write_ns.saturating_add(elapsed);
            }
            RequestOp::Flush => {
                self.counters.flushes += 1;
                self.counters.flush_ns = self.counters.flush_ns.saturating_add(elapsed);
            }
        }
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Moves with the sole completion sender from runtime admission onward.
/// Dropping a receiver cannot stop accounting. Validation and admission errors
/// precede construction. A pre-enqueue return/cancellation abandons this lease;
/// accepted requests must publish a terminal completion, even on failure.
pub(super) struct RequestAccounting {
    metrics: Arc<DeviceIoMetrics>,
    op: RequestOp,
    sectors: u64,
    started: Option<Duration>,
}

impl RequestAccounting {
    pub(super) fn new(metrics: Arc<DeviceIoMetrics>, op: RequestOp, sectors: u64) -> Self {
        let started = {
            let mut state = metrics.state.lock_irqsave();
            let now = monotonic_time();
            state.begin(now, op);
            now
        };
        Self {
            metrics,
            op,
            sectors,
            started: Some(started),
        }
    }

    pub(super) fn complete(&mut self, failed: bool) {
        if let Some(started) = self.started.take() {
            let mut state = self.metrics.state.lock_irqsave();
            state.complete(monotonic_time(), started, self.op, self.sectors, failed);
        }
    }
}

impl Drop for RequestAccounting {
    fn drop(&mut self) {
        // Only pre-enqueue returns/cancellation abandon accounting. Accepted
        // requests publish terminal completions through the hctx, including
        // shutdown/protocol failures; dropping a receiver does not own this lease.
        if self.started.take().is_some() {
            let mut state = self.metrics.state.lock_irqsave();
            state.cancel(monotonic_time(), self.op);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_rw_and_hardware_flush_account_completed_latency_and_retired_bytes() {
        let ms = Duration::from_millis;
        let mut state = IoState::default();
        state.begin(ms(10), RequestOp::Read);
        state.begin(ms(20), RequestOp::Write);
        state.begin(ms(22), RequestOp::Flush);
        assert_eq!(state.counters.inflight, 2);
        state.advance(ms(25));
        assert_eq!(state.counters.busy_ns, 15_000_000);
        assert_eq!(
            state.counters.weighted_ns, 0,
            "unfinished latency must not enter weighted time"
        );
        state.complete(ms(30), ms(10), RequestOp::Read, 8, false);
        assert_eq!(state.counters.weighted_ns, 20_000_000);
        state.complete(ms(40), ms(20), RequestOp::Write, 16, true);
        state.complete(ms(50), ms(22), RequestOp::Flush, 0, false);
        state.advance(ms(100));
        assert_eq!(state.counters.busy_ns, 30_000_000);
        assert_eq!(state.counters.weighted_ns, 68_000_000);
        assert_eq!(state.counters.flush_ns, 28_000_000);
        assert_eq!(state.counters.inflight, 0);
        assert_eq!(state.counters.read_ns, 20_000_000);
        assert_eq!(state.counters.write_ns, 20_000_000);
        assert_eq!(state.counters.sectors_read, 8);
        assert_eq!(
            state.counters.sectors_written, 16,
            "failed retirement still accounts request bytes"
        );
        assert_eq!(state.counters.failed_requests, 1);
        let completed = state.counters;
        state.begin(ms(110), RequestOp::Read);
        state.begin(ms(120), RequestOp::Write);
        state.begin(ms(125), RequestOp::Flush);
        state.cancel(ms(130), RequestOp::Write);
        assert_eq!(state.counters.inflight, 1);
        state.cancel(ms(140), RequestOp::Read);
        state.cancel(ms(150), RequestOp::Flush);
        state.advance(ms(200));
        assert_eq!(
            state.counters,
            BlockIoStats {
                busy_ns: completed.busy_ns + 30_000_000,
                ..completed
            },
            "pre-enqueue cancellation only closes the observed busy interval"
        );
    }
}
