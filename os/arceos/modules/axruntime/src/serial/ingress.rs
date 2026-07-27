use core::{
    array,
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

pub(super) const TX_FRAME_BYTES: usize = 256;
const TX_FRAME_CAPACITY: usize = 16;
const IDLE_BIT: u64 = 1;

#[derive(Clone, Copy)]
pub(super) struct TxFrame {
    epoch: u64,
    len: u16,
    bytes: [u8; TX_FRAME_BYTES],
}

impl TxFrame {
    fn new(epoch: u64, bytes: &[u8]) -> Self {
        let mut frame = Self {
            epoch,
            len: bytes.len() as u16,
            bytes: [0; TX_FRAME_BYTES],
        };
        frame.bytes[..bytes.len()].copy_from_slice(bytes);
        frame
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

struct MpscSlot {
    sequence: AtomicUsize,
    frame: UnsafeCell<MaybeUninit<TxFrame>>,
}

impl MpscSlot {
    fn new(sequence: usize) -> Self {
        Self {
            sequence: AtomicUsize::new(sequence),
            frame: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

// SAFETY: producers receive disjoint logical positions from `reserve`, and the
// sole consumer reads a slot only after its Release sequence publication.
unsafe impl Sync for MpscSlot {}

/// Fixed-capacity MPSC ring with atomic range reservation.
///
/// Reserving a range makes every multi-frame submission contiguous. Producers
/// never wait: a full ring immediately rejects the unavailable suffix. The
/// maintenance worker is the only consumer.
struct MpscFrameRing {
    slots: [MpscSlot; TX_FRAME_CAPACITY],
    enqueue: AtomicUsize,
    dequeue: AtomicUsize,
}

impl MpscFrameRing {
    fn new() -> Self {
        Self {
            slots: array::from_fn(MpscSlot::new),
            enqueue: AtomicUsize::new(0),
            dequeue: AtomicUsize::new(0),
        }
    }

    fn reserve(&self, requested: usize) -> Option<ReservedFrames<'_>> {
        debug_assert!(requested != 0);
        let mut tail = self.enqueue.load(Ordering::Relaxed);
        loop {
            let head = self.dequeue.load(Ordering::Acquire);
            let used = tail.wrapping_sub(head);
            debug_assert!(used <= TX_FRAME_CAPACITY);
            let count = requested.min(TX_FRAME_CAPACITY - used);
            if count == 0 {
                return None;
            }
            match self.enqueue.compare_exchange_weak(
                tail,
                tail.wrapping_add(count),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ReservedFrames {
                        ring: self,
                        start: tail,
                        count,
                    });
                }
                Err(observed) => tail = observed,
            }
        }
    }

    fn pop(&self) -> Option<TxFrame> {
        let head = self.dequeue.load(Ordering::Relaxed);
        let slot = &self.slots[head % TX_FRAME_CAPACITY];
        if slot.sequence.load(Ordering::Acquire) != head.wrapping_add(1) {
            return None;
        }
        // SAFETY: the matching sequence proves that one producer initialized
        // this slot and Release-published exclusive ownership to the consumer.
        let frame = unsafe { (*slot.frame.get()).assume_init_read() };
        slot.sequence
            .store(head.wrapping_add(TX_FRAME_CAPACITY), Ordering::Release);
        self.dequeue.store(head.wrapping_add(1), Ordering::Release);
        Some(frame)
    }

    fn len(&self) -> usize {
        self.enqueue
            .load(Ordering::Acquire)
            .wrapping_sub(self.dequeue.load(Ordering::Acquire))
            .min(TX_FRAME_CAPACITY)
    }

    fn has_pending(&self) -> bool {
        let head = self.dequeue.load(Ordering::Relaxed);
        self.slots[head % TX_FRAME_CAPACITY]
            .sequence
            .load(Ordering::Acquire)
            == head.wrapping_add(1)
    }

    fn has_reserved(&self) -> bool {
        self.enqueue.load(Ordering::Acquire) != self.dequeue.load(Ordering::Acquire)
    }
}

impl Drop for MpscFrameRing {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}

struct ReservedFrames<'a> {
    ring: &'a MpscFrameRing,
    start: usize,
    count: usize,
}

impl ReservedFrames<'_> {
    fn count(&self) -> usize {
        self.count
    }

    fn publish(&self, offset: usize, frame: TxFrame) {
        assert!(offset < self.count, "reserved TX frame offset is valid");
        let position = self.start.wrapping_add(offset);
        let slot = &self.ring.slots[position % TX_FRAME_CAPACITY];
        debug_assert_eq!(slot.sequence.load(Ordering::Acquire), position);
        // SAFETY: the successful range reservation gives this producer
        // exclusive ownership of `position` until the sequence store.
        unsafe { (*slot.frame.get()).write(frame) };
        slot.sequence
            .store(position.wrapping_add(1), Ordering::Release);
    }
}

/// Lock-free fixed-capacity normal-TX ingress.
///
/// Producers can run on any CPU and never disable IRQs, allocate, or wait.
/// Start/stop epochs allow the sole worker to discard submissions that race a
/// lifecycle transition without reintroducing a shared queue lock.
pub(super) struct TxIngress {
    ring: MpscFrameRing,
    accepting: AtomicBool,
    epoch: AtomicU64,
    activity: AtomicU64,
}

impl TxIngress {
    pub(super) fn new() -> Self {
        Self {
            ring: MpscFrameRing::new(),
            accepting: AtomicBool::new(false),
            epoch: AtomicU64::new(0),
            activity: AtomicU64::new(IDLE_BIT),
        }
    }

    pub(super) fn try_write(&self, bytes: &[u8], notify: impl FnOnce()) -> usize {
        if bytes.is_empty() || !self.accepting.load(Ordering::Acquire) {
            return 0;
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        if !self.accepting.load(Ordering::Acquire) {
            return 0;
        }

        self.publish_activity(false);
        let requested = bytes.len().div_ceil(TX_FRAME_BYTES);
        let Some(reservation) = self.ring.reserve(requested) else {
            return 0;
        };
        self.publish_activity(false);

        let mut accepted = 0;
        for offset in 0..reservation.count() {
            let end = (accepted + TX_FRAME_BYTES).min(bytes.len());
            reservation.publish(offset, TxFrame::new(epoch, &bytes[accepted..end]));
            accepted = end;
        }
        notify();
        accepted
    }

    pub(super) fn try_write_log(&self, bytes: &[u8], notify: impl FnOnce()) -> usize {
        if bytes.is_empty() || !self.accepting.load(Ordering::Acquire) {
            return 0;
        }
        let epoch = self.epoch.load(Ordering::Acquire);
        if !self.accepting.load(Ordering::Acquire) {
            return 0;
        }

        self.publish_activity(false);
        let Some(reservation) = self.ring.reserve(text_frame_count(bytes)) else {
            return 0;
        };
        self.publish_activity(false);

        let accepted = publish_text_frames(&reservation, epoch, bytes);
        notify();
        accepted
    }

    pub(super) fn pop(&self) -> Option<TxFrame> {
        let epoch = self.epoch.load(Ordering::Acquire);
        loop {
            let frame = self.ring.pop()?;
            if frame.epoch == epoch {
                return Some(frame);
            }
        }
    }

    pub(super) fn has_pending(&self) -> bool {
        self.ring.has_pending()
    }

    pub(super) fn start_accepting(&self) {
        self.accepting.store(false, Ordering::Release);
        self.advance_epoch();
        while self.ring.pop().is_some() {}
        self.publish_activity(true);
        self.accepting.store(true, Ordering::Release);
    }

    pub(super) fn stop_and_discard(&self) {
        self.accepting.store(false, Ordering::Release);
        self.advance_epoch();
        while self.ring.pop().is_some() {}
        self.publish_activity(true);
    }

    pub(super) fn write_room(&self) -> usize {
        if !self.accepting.load(Ordering::Acquire) {
            return 0;
        }
        (TX_FRAME_CAPACITY - self.ring.len()) * TX_FRAME_BYTES
    }

    pub(super) fn is_idle(&self) -> bool {
        self.activity.load(Ordering::Acquire) & IDLE_BIT != 0
    }

    /// Publishes idle only if no producer changed activity around the check.
    pub(super) fn mark_idle_if_empty(&self, worker_empty: bool, hardware_idle: bool) -> bool {
        if !worker_empty || !hardware_idle || self.ring.has_reserved() {
            return false;
        }
        let observed = self.activity.load(Ordering::Acquire);
        if observed & IDLE_BIT != 0 {
            return false;
        }
        if self.ring.has_reserved() {
            return false;
        }
        self.activity
            .compare_exchange(
                observed,
                observed | IDLE_BIT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn advance_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    fn publish_activity(&self, idle: bool) {
        let _ = self
            .activity
            .try_update(Ordering::AcqRel, Ordering::Acquire, |activity| {
                let generation = (activity & !IDLE_BIT).wrapping_add(2) & !IDLE_BIT;
                Some(generation | u64::from(idle))
            });
    }
}

fn text_frame_count(bytes: &[u8]) -> usize {
    let mut frames = 1;
    let mut frame_len = 0;
    for &byte in bytes {
        let encoded_len = if byte == b'\n' { 2 } else { 1 };
        if frame_len + encoded_len > TX_FRAME_BYTES {
            frames += 1;
            frame_len = 0;
        }
        frame_len += encoded_len;
    }
    frames
}

fn publish_text_frames(reservation: &ReservedFrames<'_>, epoch: u64, bytes: &[u8]) -> usize {
    let mut accepted = 0;
    for offset in 0..reservation.count() {
        let mut encoded = [0; TX_FRAME_BYTES];
        let mut encoded_len = 0;
        while accepted < bytes.len() {
            let byte = bytes[accepted];
            let required = if byte == b'\n' { 2 } else { 1 };
            if encoded_len + required > encoded.len() {
                break;
            }
            if byte == b'\n' {
                encoded[encoded_len] = b'\r';
                encoded_len += 1;
            }
            encoded[encoded_len] = byte;
            encoded_len += 1;
            accepted += 1;
        }
        debug_assert!(encoded_len != 0);
        reservation.publish(offset, TxFrame::new(epoch, &encoded[..encoded_len]));
    }
    accepted
}

pub(super) struct TxFrameCursor {
    frame: TxFrame,
    offset: usize,
}

impl TxFrameCursor {
    pub(super) fn new(frame: TxFrame) -> Self {
        Self { frame, offset: 0 }
    }

    pub(super) fn remaining(&self) -> &[u8] {
        &self.frame.bytes()[self.offset..]
    }

    pub(super) fn advance(&mut self, count: usize) {
        self.offset += count;
    }

    pub(super) fn is_complete(&self) -> bool {
        self.offset == self.frame.bytes().len()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{sync::Arc, vec::Vec};
    use std::{sync::Barrier, thread};

    use super::*;

    fn started_ingress() -> TxIngress {
        let ingress = TxIngress::new();
        ingress.start_accepting();
        ingress
    }

    #[test]
    fn text_log_submission_expands_line_feeds_to_crlf() {
        let ingress = started_ingress();

        assert_eq!(ingress.try_write_log(b"first\nsecond\n", || {}), 13);

        let mut output = Vec::new();
        while let Some(frame) = ingress.pop() {
            output.extend_from_slice(frame.bytes());
        }
        assert_eq!(output, b"first\r\nsecond\r\n");
    }

    #[test]
    fn text_encoding_keeps_crlf_in_one_frame() {
        let ingress = started_ingress();
        let mut input = Vec::from([b'x'; TX_FRAME_BYTES - 1]);
        input.push(b'\n');

        assert_eq!(ingress.try_write_log(&input, || {}), input.len());
        assert_eq!(ingress.pop().unwrap().bytes(), &[b'x'; TX_FRAME_BYTES - 1]);
        assert_eq!(ingress.pop().unwrap().bytes(), b"\r\n");
        assert!(ingress.pop().is_none());
    }

    #[test]
    fn sequential_submissions_preserve_order() {
        let ingress = started_ingress();
        assert_eq!(ingress.try_write(b"first", || {}), 5);
        assert_eq!(ingress.try_write(b"second", || {}), 6);
        assert_eq!(ingress.pop().unwrap().bytes(), b"first");
        assert_eq!(ingress.pop().unwrap().bytes(), b"second");
    }

    #[test]
    fn queue_accepts_partial_input_at_its_fixed_capacity() {
        let ingress = started_ingress();
        let bytes = [0x55; TX_FRAME_BYTES * (TX_FRAME_CAPACITY + 1)];

        assert_eq!(
            ingress.try_write(&bytes, || {}),
            TX_FRAME_BYTES * TX_FRAME_CAPACITY
        );
        assert_eq!(ingress.try_write(b"x", || {}), 0);
    }

    #[test]
    fn concurrent_multi_frame_reservations_do_not_interleave() {
        let ingress = Arc::new(started_ingress());
        let start = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for byte in [b'a', b'b'] {
            let ingress = ingress.clone();
            let start = start.clone();
            threads.push(thread::spawn(move || {
                let bytes = [byte; TX_FRAME_BYTES + 1];
                start.wait();
                assert_eq!(ingress.try_write(&bytes, || {}), bytes.len());
            }));
        }
        start.wait();
        for thread in threads {
            thread.join().unwrap();
        }

        let mut labels = Vec::new();
        while let Some(frame) = ingress.pop() {
            labels.push(frame.bytes()[0]);
        }
        assert!(labels == [b'a', b'a', b'b', b'b'] || labels == [b'b', b'b', b'a', b'a']);
    }

    #[test]
    fn reserved_head_is_not_reported_ready_before_publication() {
        let ring = MpscFrameRing::new();
        let reservation = ring.reserve(1).expect("one free frame");

        assert!(
            !ring.has_pending(),
            "the worker must not spin on a producer reservation that has not been published"
        );
        reservation.publish(0, TxFrame::new(0, b"x"));
        assert!(ring.has_pending());
        assert_eq!(ring.pop().unwrap().bytes(), b"x");
    }

    #[test]
    fn lifecycle_epoch_discards_frames_from_the_previous_start() {
        let ingress = started_ingress();
        assert_eq!(ingress.try_write(b"old", || {}), 3);
        ingress.stop_and_discard();
        ingress.start_accepting();
        assert_eq!(ingress.try_write(b"new", || {}), 3);

        assert_eq!(ingress.pop().unwrap().bytes(), b"new");
        assert!(ingress.pop().is_none());
    }

    #[test]
    fn a_submit_after_idle_publication_clears_idle_again() {
        let ingress = started_ingress();
        ingress.publish_activity(false);
        assert!(ingress.mark_idle_if_empty(true, true));
        assert!(ingress.is_idle());

        assert_eq!(ingress.try_write(b"x", || {}), 1);
        assert!(!ingress.is_idle());
    }

    #[test]
    fn hard_path_activity_generation_wraps_without_panicking() {
        let ingress = TxIngress::new();
        ingress.activity.store(u64::MAX - 1, Ordering::Release);

        ingress.publish_activity(false);

        assert_eq!(ingress.activity.load(Ordering::Acquire), 0);
    }

    #[test]
    fn lifecycle_epoch_wraps_after_the_old_ring_is_drained() {
        let ingress = TxIngress::new();
        ingress.epoch.store(u64::MAX, Ordering::Release);

        ingress.advance_epoch();

        assert_eq!(ingress.epoch.load(Ordering::Acquire), 0);
    }
}
