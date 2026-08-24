//! Per-backend bounded queues kept off the console control-state lock.

use alloc::{collections::VecDeque, format, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_std::os::arceos::{modules::ax_task::IrqNotify, sync::IrqSafeMutex};

use super::INPUT_QUEUE_CAPACITY;

const OUTPUT_QUEUE_CAPACITY: usize = 64 * 1024;
const OUTPUT_DRAIN_BATCH: usize = 4 * 1024;

pub(super) struct GuestConsoleEndpoint {
    active: AtomicBool,
    output_ready: &'static IrqNotify,
    input: IrqSafeMutex<ByteRing>,
    output: IrqSafeMutex<ByteRing>,
    input_overflow_reported: AtomicBool,
    input_enqueued: AtomicUsize,
    input_drained: AtomicUsize,
    input_dropped: AtomicUsize,
    output_enqueued: AtomicUsize,
    output_drained: AtomicUsize,
    output_dropped: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct EndpointSnapshot {
    pub(super) active: bool,
    pub(super) input_enqueued: usize,
    pub(super) input_drained: usize,
    pub(super) input_dropped: usize,
    pub(super) input_pending: usize,
    pub(super) output_enqueued: usize,
    pub(super) output_drained: usize,
    pub(super) output_dropped: usize,
    pub(super) output_pending: usize,
}

impl GuestConsoleEndpoint {
    pub(super) fn new(output_ready: &'static IrqNotify) -> Self {
        Self {
            active: AtomicBool::new(true),
            output_ready,
            input: IrqSafeMutex::new(ByteRing::new(INPUT_QUEUE_CAPACITY)),
            output: IrqSafeMutex::new(ByteRing::new(OUTPUT_QUEUE_CAPACITY)),
            input_overflow_reported: AtomicBool::new(false),
            input_enqueued: AtomicUsize::new(0),
            input_drained: AtomicUsize::new(0),
            input_dropped: AtomicUsize::new(0),
            output_enqueued: AtomicUsize::new(0),
            output_drained: AtomicUsize::new(0),
            output_dropped: AtomicUsize::new(0),
        }
    }

    /// Queues guest input and reports the first overflow since the last drain.
    pub(super) fn push_input(&self, bytes: &[u8]) -> bool {
        if bytes.is_empty() || !self.active.load(Ordering::Acquire) {
            return false;
        }
        let mut input = self.input.lock();
        if !self.active.load(Ordering::Acquire) {
            return false;
        }
        let dropped = input.push(bytes);
        self.input_enqueued
            .fetch_add(bytes.len(), Ordering::Relaxed);
        self.input_dropped.fetch_add(dropped, Ordering::Relaxed);
        dropped != 0 && !self.input_overflow_reported.swap(true, Ordering::AcqRel)
    }

    pub(super) fn read_input(&self, buffer: &mut [u8]) -> usize {
        if !self.active.load(Ordering::Acquire) {
            return 0;
        }
        let count = self.input.lock().read(buffer);
        self.input_drained.fetch_add(count, Ordering::Relaxed);
        if count != 0 {
            self.input_overflow_reported.store(false, Ordering::Release);
        }
        count
    }

    pub(super) fn write_output(&self, bytes: &[u8]) {
        if bytes.is_empty() || !self.active.load(Ordering::Acquire) {
            return;
        }
        let submitted = {
            let mut output = self.output.lock();
            if !self.active.load(Ordering::Acquire) {
                return;
            }
            let dropped = output.push(bytes);
            self.output_enqueued
                .fetch_add(bytes.len(), Ordering::Relaxed);
            self.output_dropped.fetch_add(dropped, Ordering::Relaxed);
            true
        };
        if submitted {
            self.output_ready.notify_irq();
        }
    }

    pub(super) fn take_output(&self) -> EndpointOutputBatch {
        let mut bytes = Vec::with_capacity(OUTPUT_DRAIN_BATCH);
        let dropped_bytes = {
            let mut output = self.output.lock();
            let dropped_bytes = core::mem::take(&mut output.dropped_bytes);
            output.read_into(&mut bytes, OUTPUT_DRAIN_BATCH);
            dropped_bytes
        };
        self.output_drained
            .fetch_add(bytes.len(), Ordering::Relaxed);
        EndpointOutputBatch {
            bytes,
            dropped_bytes,
        }
    }

    pub(super) fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
        self.input_overflow_reported.store(false, Ordering::Release);
        self.input.lock().clear();
        self.output.lock().clear();
    }

    pub(super) fn has_output(&self) -> bool {
        let output = self.output.lock();
        !output.pending.is_empty() || output.dropped_bytes != 0
    }

    pub(super) fn snapshot(&self) -> EndpointSnapshot {
        let input_pending = self.input.lock().pending.len();
        let output_pending = self.output.lock().pending.len();
        EndpointSnapshot {
            active: self.active.load(Ordering::Relaxed),
            input_enqueued: self.input_enqueued.load(Ordering::Relaxed),
            input_drained: self.input_drained.load(Ordering::Relaxed),
            input_dropped: self.input_dropped.load(Ordering::Relaxed),
            input_pending,
            output_enqueued: self.output_enqueued.load(Ordering::Relaxed),
            output_drained: self.output_drained.load(Ordering::Relaxed),
            output_dropped: self.output_dropped.load(Ordering::Relaxed),
            output_pending,
        }
    }
}

impl core::fmt::Debug for GuestConsoleEndpoint {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("GuestConsoleEndpoint")
            .field("active", &self.active.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

pub(super) struct EndpointOutputBatch {
    bytes: Vec<u8>,
    dropped_bytes: usize,
}

impl EndpointOutputBatch {
    pub(super) fn is_empty(&self) -> bool {
        self.bytes.is_empty() && self.dropped_bytes == 0
    }

    pub(super) fn into_bytes(self) -> Vec<u8> {
        if self.dropped_bytes == 0 {
            return self.bytes;
        }
        let marker = format!(
            "\n[Axvisor] guest console ingress dropped {} bytes\n",
            self.dropped_bytes
        );
        let mut output = Vec::with_capacity(marker.len().saturating_add(self.bytes.len()));
        output.extend_from_slice(marker.as_bytes());
        output.extend_from_slice(&self.bytes);
        output
    }
}

struct ByteRing {
    pending: VecDeque<u8>,
    capacity: usize,
    dropped_bytes: usize,
}

impl ByteRing {
    fn new(capacity: usize) -> Self {
        Self {
            pending: VecDeque::with_capacity(capacity),
            capacity,
            dropped_bytes: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> usize {
        let dropped_before = self.dropped_bytes;
        if bytes.len() >= self.capacity {
            self.dropped_bytes = self
                .dropped_bytes
                .saturating_add(self.pending.len())
                .saturating_add(bytes.len() - self.capacity);
            self.pending.clear();
            self.pending
                .extend(bytes[bytes.len() - self.capacity..].iter().copied());
            return self.dropped_bytes.saturating_sub(dropped_before);
        }
        for &byte in bytes {
            if self.pending.len() == self.capacity {
                self.pending.pop_front();
                self.dropped_bytes = self.dropped_bytes.saturating_add(1);
            }
            self.pending.push_back(byte);
        }
        self.dropped_bytes.saturating_sub(dropped_before)
    }

    fn read(&mut self, buffer: &mut [u8]) -> usize {
        let count = buffer.len().min(self.pending.len());
        for byte in &mut buffer[..count] {
            *byte = self
                .pending
                .pop_front()
                .expect("byte ring length was checked");
        }
        count
    }

    fn read_into(&mut self, output: &mut Vec<u8>, limit: usize) {
        let count = limit.min(self.pending.len());
        output.extend(self.pending.drain(..count));
    }

    fn clear(&mut self) {
        self.pending.clear();
        self.dropped_bytes = 0;
    }
}

#[cfg(any(test, axtest))]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn output_ring_is_bounded_and_reports_overwrite() {
        static READY: IrqNotify = IrqNotify::new();
        let endpoint = GuestConsoleEndpoint::new(&READY);
        endpoint.write_output(&vec![b'a'; OUTPUT_QUEUE_CAPACITY]);
        endpoint.write_output(b"tail");

        let output = endpoint.take_output().into_bytes();

        assert!(
            output
                .windows(b"ingress dropped 4 bytes".len())
                .any(|window| window == b"ingress dropped 4 bytes")
        );
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn deactivated_endpoint_rejects_late_io() {
        static READY: IrqNotify = IrqNotify::new();
        let endpoint = GuestConsoleEndpoint::new(&READY);
        endpoint.push_input(b"input");
        endpoint.write_output(b"output");
        endpoint.deactivate();
        endpoint.push_input(b"late input");
        endpoint.write_output(b"late output");

        assert_eq!(endpoint.read_input(&mut [0; 16]), 0);
        assert!(endpoint.take_output().is_empty());
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn output_snapshot_tracks_enqueue_drain_and_pending_bytes() {
        static READY: IrqNotify = IrqNotify::new();
        let endpoint = GuestConsoleEndpoint::new(&READY);
        endpoint.write_output(b"hello");
        let queued = endpoint.snapshot();
        assert_eq!(queued.output_enqueued, 5);
        assert_eq!(queued.output_pending, 5);

        assert_eq!(endpoint.take_output().into_bytes(), b"hello");
        let drained = endpoint.snapshot();
        assert_eq!(drained.output_drained, 5);
        assert_eq!(drained.output_pending, 0);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn input_snapshot_tracks_enqueue_drain_drop_and_pending_bytes() {
        static READY: IrqNotify = IrqNotify::new();
        let endpoint = GuestConsoleEndpoint::new(&READY);
        endpoint.push_input(&vec![b'a'; INPUT_QUEUE_CAPACITY]);
        endpoint.push_input(b"tail");

        let queued = endpoint.snapshot();
        assert_eq!(queued.input_enqueued, INPUT_QUEUE_CAPACITY + 4);
        assert_eq!(queued.input_dropped, 4);
        assert_eq!(queued.input_pending, INPUT_QUEUE_CAPACITY);

        let mut input = [0; 8];
        assert_eq!(endpoint.read_input(&mut input), input.len());
        let drained = endpoint.snapshot();
        assert_eq!(drained.input_drained, input.len());
        assert_eq!(drained.input_pending, INPUT_QUEUE_CAPACITY - input.len());
    }
}
