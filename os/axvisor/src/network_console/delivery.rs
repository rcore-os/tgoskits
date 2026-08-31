//! Browser frame coalescing owned by one console dispatcher.

use std::vec::Vec;

use ax_std::os::arceos::modules::ax_task::IrqNotify;
use axvisor::console_mux::HostOutputQueue;

/// Coalescing notification that blocks consumers until a producer publishes work.
pub(crate) struct BlockingSignal {
    notify: IrqNotify,
}

impl BlockingSignal {
    pub(crate) const fn new() -> Self {
        Self {
            notify: IrqNotify::new(),
        }
    }

    pub(crate) fn notify_irq(&self) {
        if !self.notify.is_pending() {
            self.notify.notify_irq();
        }
    }

    pub(crate) fn notify(&self) {
        if !self.notify.is_pending() {
            self.notify.notify();
        }
    }

    pub(crate) fn drain(&self) {
        self.notify.drain();
    }

    pub(crate) fn wait(&self) {
        self.notify.wait();
    }
}

/// Fixed-capacity handoff from a console dispatcher to its WebSocket writer.
pub(crate) struct DeliveryQueue<const CAPACITY: usize> {
    queue: HostOutputQueue<CAPACITY>,
}

impl<const CAPACITY: usize> DeliveryQueue<CAPACITY> {
    pub(crate) const fn new() -> Self {
        Self {
            queue: HostOutputQueue::new(),
        }
    }

    pub(crate) fn enqueue(&mut self, bytes: &[u8]) {
        self.queue.enqueue(bytes);
    }

    /// Returns preserved bytes and the complete transactions dropped since
    /// the preceding read.
    pub(crate) fn dequeue(&mut self, output: &mut [u8]) -> (usize, usize) {
        let dropped_bytes = self.queue.take_dropped_bytes();
        let len = self.queue.dequeue(output);
        (len, dropped_bytes)
    }
}

/// One WebSocket frame assembled before crossing the bounded delivery channel.
pub(crate) struct DeliveryFrame {
    bytes: Vec<u8>,
}

impl DeliveryFrame {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn append(&mut self, bytes: &[u8], dropped_bytes: usize) {
        if dropped_bytes != 0 {
            self.bytes.extend_from_slice(
                format!("\r\n[Axvisor browser console dropped {dropped_bytes} queued bytes]\r\n")
                    .as_bytes(),
            );
        }
        self.bytes.extend_from_slice(bytes);
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
