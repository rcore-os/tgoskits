//! Browser frame coalescing owned by one console dispatcher.

use std::vec::Vec;

use axvisor::console_mux::HostOutputQueue;

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

#[cfg(any(test, axtest))]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn frame_coalesces_ordered_dispatcher_batches() {
        let mut delivery = DeliveryFrame::with_capacity(16);

        delivery.append(b"starry ", 0);
        delivery.append(b"continues", 0);

        assert_eq!(delivery.into_bytes(), b"starry continues");
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn frame_reports_source_queue_overflow_before_preserved_bytes() {
        let mut delivery = DeliveryFrame::with_capacity(96);

        delivery.append(b"preserved", 11);

        let output = delivery.into_bytes();
        assert!(output.starts_with(b"\r\n[Axvisor browser console dropped 11 queued bytes]\r\n"));
        assert!(output.ends_with(b"preserved"));
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn queue_preserves_old_output_and_reports_new_overflow() {
        let mut delivery = DeliveryQueue::<8>::new();
        delivery.enqueue(b"old");
        delivery.enqueue(b"overflow");

        let mut output = [0; 8];
        let (len, dropped_bytes) = delivery.dequeue(&mut output);
        assert_eq!(&output[..len], b"old");
        assert_eq!(dropped_bytes, 8);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn waiter_blocks_for_notification_without_timer_polling() {
        use core::sync::atomic::{AtomicBool, Ordering};
        use std::{sync::Arc, thread, time::Duration};

        use ax_std::os::arceos::modules::ax_runtime::task::sync::irq::{
            IrqWaitCell, IrqWorkerWaiter,
        };
        use ax_std::os::arceos::modules::ax_runtime::task::thread::current::current_thread_handle;

        let signal = Arc::new(IrqWaitCell::new());
        let waiting = Arc::new(AtomicBool::new(false));
        let woke = Arc::new(AtomicBool::new(false));
        let worker_signal = Arc::clone(&signal);
        let worker_waiting = Arc::clone(&waiting);
        let worker_woke = Arc::clone(&woke);
        let worker = thread::spawn(move || {
            let current =
                current_thread_handle().expect("delivery waiter must bind to its runtime worker");
            let waiter = IrqWorkerWaiter::new(current.wake_handle());
            worker_waiting.store(true, Ordering::Release);
            waiter
                .wait(&worker_signal)
                .expect("delivery waiter must accept one notification cell");
            worker_woke.store(true, Ordering::Release);
        });

        while !waiting.load(Ordering::Acquire) {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(30));
        assert!(!woke.load(Ordering::Acquire));

        let _result = signal.notify();
        worker
            .join()
            .expect("delivery waiter must exit after notify");
        assert!(woke.load(Ordering::Acquire));
    }
}
