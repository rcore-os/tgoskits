use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::Dwc2TransferFault;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Dwc2TransferStats {
    pub transfers: usize,
    pub stages: usize,
    pub dma_allocs: usize,
    pub bounce_to_device_bytes: usize,
    pub bounce_from_device_bytes: usize,
    pub naks: usize,
    pub xact_errors: usize,
    pub timeouts: usize,
    pub wait_iters: usize,
    pub init_wait_iters: usize,
    pub transfer_busy_wait_iters: usize,
    pub irq_events: usize,
    pub channel_completions: usize,
}

#[derive(Default)]
pub(crate) struct Dwc2StatsInner {
    transfers: AtomicUsize,
    stages: AtomicUsize,
    dma_allocs: AtomicUsize,
    bounce_to_device_bytes: AtomicUsize,
    bounce_from_device_bytes: AtomicUsize,
    naks: AtomicUsize,
    xact_errors: AtomicUsize,
    timeouts: AtomicUsize,
    init_wait_iters: AtomicUsize,
    transfer_busy_wait_iters: AtomicUsize,
    irq_events: AtomicUsize,
    channel_completions: AtomicUsize,
}

#[derive(Clone, Default)]
pub(crate) struct Dwc2Stats {
    inner: Arc<Dwc2StatsInner>,
}

impl Dwc2Stats {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn reset(&self) {
        self.inner.transfers.store(0, Ordering::Relaxed);
        self.inner.stages.store(0, Ordering::Relaxed);
        self.inner.dma_allocs.store(0, Ordering::Relaxed);
        self.inner
            .bounce_to_device_bytes
            .store(0, Ordering::Relaxed);
        self.inner
            .bounce_from_device_bytes
            .store(0, Ordering::Relaxed);
        self.inner.naks.store(0, Ordering::Relaxed);
        self.inner.xact_errors.store(0, Ordering::Relaxed);
        self.inner.timeouts.store(0, Ordering::Relaxed);
        self.inner.init_wait_iters.store(0, Ordering::Relaxed);
        self.inner
            .transfer_busy_wait_iters
            .store(0, Ordering::Relaxed);
        self.inner.irq_events.store(0, Ordering::Relaxed);
        self.inner.channel_completions.store(0, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> Dwc2TransferStats {
        let init_wait_iters = self.inner.init_wait_iters.load(Ordering::Relaxed);
        let transfer_busy_wait_iters = self.inner.transfer_busy_wait_iters.load(Ordering::Relaxed);
        Dwc2TransferStats {
            transfers: self.inner.transfers.load(Ordering::Relaxed),
            stages: self.inner.stages.load(Ordering::Relaxed),
            dma_allocs: self.inner.dma_allocs.load(Ordering::Relaxed),
            bounce_to_device_bytes: self.inner.bounce_to_device_bytes.load(Ordering::Relaxed),
            bounce_from_device_bytes: self.inner.bounce_from_device_bytes.load(Ordering::Relaxed),
            naks: self.inner.naks.load(Ordering::Relaxed),
            xact_errors: self.inner.xact_errors.load(Ordering::Relaxed),
            timeouts: self.inner.timeouts.load(Ordering::Relaxed),
            wait_iters: init_wait_iters + transfer_busy_wait_iters,
            init_wait_iters,
            transfer_busy_wait_iters,
            irq_events: self.inner.irq_events.load(Ordering::Relaxed),
            channel_completions: self.inner.channel_completions.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn record_transfer(&self) {
        self.inner.transfers.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_stage(&self) {
        self.inner.stages.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_dma_alloc(&self) {
        self.inner.dma_allocs.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_bounce_to_device(&self, bytes: usize) {
        self.inner
            .bounce_to_device_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn record_bounce_from_device(&self, bytes: usize) {
        self.inner
            .bounce_from_device_bytes
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub(crate) fn record_fault(&self, fault: Dwc2TransferFault) {
        match fault {
            Dwc2TransferFault::Nak => {
                self.inner.naks.fetch_add(1, Ordering::Relaxed);
            }
            Dwc2TransferFault::Xact => {
                self.inner.xact_errors.fetch_add(1, Ordering::Relaxed);
            }
            Dwc2TransferFault::Stall
            | Dwc2TransferFault::Ahb
            | Dwc2TransferFault::Babble
            | Dwc2TransferFault::FrameOverrun
            | Dwc2TransferFault::DataToggle
            | Dwc2TransferFault::HaltedWithoutComplete => {}
        }
    }

    pub(crate) fn record_timeout(&self) {
        self.inner.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_init_wait_iters(&self, iters: usize) {
        self.inner
            .init_wait_iters
            .fetch_add(iters, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn record_transfer_busy_wait_iters(&self, iters: usize) {
        self.inner
            .transfer_busy_wait_iters
            .fetch_add(iters, Ordering::Relaxed);
    }

    pub(crate) fn record_irq_event(&self) {
        self.inner.irq_events.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_channel_completion(&self) {
        self.inner
            .channel_completions
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn stats_records_transfer_faults_and_wait_iterations() {
        let stats = Dwc2Stats::new();

        stats.record_transfer();
        stats.record_stage();
        stats.record_dma_alloc();
        stats.record_bounce_to_device(9);
        stats.record_bounce_from_device(9);
        stats.record_fault(Dwc2TransferFault::Nak);
        stats.record_fault(Dwc2TransferFault::Xact);
        stats.record_timeout();
        stats.record_transfer_busy_wait_iters(17);
        stats.record_init_wait_iters(3);
        stats.record_irq_event();
        stats.record_channel_completion();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.transfers, 1);
        assert_eq!(snapshot.stages, 1);
        assert_eq!(snapshot.dma_allocs, 1);
        assert_eq!(snapshot.bounce_to_device_bytes, 9);
        assert_eq!(snapshot.bounce_from_device_bytes, 9);
        assert_eq!(snapshot.naks, 1);
        assert_eq!(snapshot.xact_errors, 1);
        assert_eq!(snapshot.timeouts, 1);
        assert_eq!(snapshot.transfer_busy_wait_iters, 17);
        assert_eq!(snapshot.init_wait_iters, 3);
        assert_eq!(snapshot.wait_iters, 20);
        assert_eq!(snapshot.irq_events, 1);
        assert_eq!(snapshot.channel_completions, 1);
    }
}
