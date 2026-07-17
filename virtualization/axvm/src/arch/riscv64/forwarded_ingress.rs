// Preallocated hard-IRQ ingress for physical interrupt ownership transfer.

use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

pub(crate) const FORWARDED_IRQ_DRAIN_BATCH: usize = 64;
const IRQ_WORD_BITS: usize = u64::BITS as usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ForwardedIrqPublish {
    WakeOwner,
    Coalesced,
    Fault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ForwardedIrqEntry {
    source: u32,
    claim: u64,
}

impl ForwardedIrqEntry {
    const EMPTY: Self = Self {
        source: 0,
        claim: 0,
    };

    pub(crate) const fn source(self) -> usize {
        self.source as usize
    }

    pub(crate) const fn claim(self) -> u64 {
        self.claim
    }
}

pub(crate) struct ForwardedIrqBatch {
    entries: [ForwardedIrqEntry; FORWARDED_IRQ_DRAIN_BATCH],
    len: usize,
}

impl ForwardedIrqBatch {
    pub(crate) fn entries(&self) -> &[ForwardedIrqEntry] {
        &self.entries[..self.len]
    }
}

/// Fixed-capacity ingress shared by a hard-IRQ producer and one owner thread.
///
/// Publication is strictly `claim slot -> pending word -> notification bit`.
/// The producer performs no allocation, free, logging, callback, or lock
/// acquisition. The owner merges at most [`FORWARDED_IRQ_DRAIN_BATCH`] entries
/// into the software interrupt controller per safe tail.
pub(crate) struct ForwardedIrqIngress {
    claims: Box<[AtomicU64]>,
    collision_retries: Box<[AtomicU8]>,
    pending_words: Box<[AtomicU64]>,
    notification_armed: AtomicBool,
    faults: AtomicUsize,
}

impl ForwardedIrqIngress {
    pub(crate) fn new(source_count: usize) -> Self {
        assert!(source_count > 1, "IRQ ingress requires nonzero source IDs");
        let word_count = source_count.div_ceil(IRQ_WORD_BITS);
        Self {
            claims: (0..source_count)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            collision_retries: (0..source_count)
                .map(|_| AtomicU8::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            pending_words: (0..word_count)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            notification_armed: AtomicBool::new(false),
            faults: AtomicUsize::new(0),
        }
    }

    /// Publishes one pre-encoded physical claim from hard-IRQ context.
    pub(crate) fn publish(&self, source: usize, claim: u64) -> ForwardedIrqPublish {
        if source == 0 || source >= self.claims.len() || claim == 0 {
            self.record_fault();
            return ForwardedIrqPublish::Fault;
        }
        if self.claims[source]
            .compare_exchange(0, claim, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.record_fault();
            return ForwardedIrqPublish::Fault;
        }

        let (word, bit) = pending_location(source);
        self.pending_words[word].fetch_or(bit, Ordering::Release);
        if self.notification_armed.swap(true, Ordering::AcqRel) {
            ForwardedIrqPublish::Coalesced
        } else {
            ForwardedIrqPublish::WakeOwner
        }
    }

    /// Takes at most one bounded owner-thread batch with Acquire observation.
    pub(crate) fn take_batch(&self) -> ForwardedIrqBatch {
        let mut batch = ForwardedIrqBatch {
            entries: [ForwardedIrqEntry::EMPTY; FORWARDED_IRQ_DRAIN_BATCH],
            len: 0,
        };

        for (word_index, pending_word) in self.pending_words.iter().enumerate() {
            let mut pending = pending_word.swap(0, Ordering::AcqRel);
            while pending != 0 && batch.len < FORWARDED_IRQ_DRAIN_BATCH {
                let bit_index = pending.trailing_zeros() as usize;
                let bit = 1u64 << bit_index;
                pending &= !bit;
                let source = word_index * IRQ_WORD_BITS + bit_index;
                let claim = self.claims[source].load(Ordering::Acquire);
                if claim == 0 {
                    self.record_fault();
                    continue;
                }
                batch.entries[batch.len] = ForwardedIrqEntry {
                    source: source as u32,
                    claim,
                };
                batch.len += 1;
            }
            if pending != 0 {
                pending_word.fetch_or(pending, Ordering::Release);
                break;
            }
            if batch.len == FORWARDED_IRQ_DRAIN_BATCH {
                break;
            }
        }
        batch
    }

    /// Restores one entry that could not yet be merged by the owner.
    pub(crate) fn requeue(&self, source: usize) {
        if source == 0 || source >= self.claims.len() {
            self.record_fault();
            return;
        }
        let (word, bit) = pending_location(source);
        self.pending_words[word].fetch_or(bit, Ordering::Release);
    }

    /// Completes the notification handshake after an owner batch.
    ///
    /// `true` asks the caller to wake the owner again because a producer raced
    /// with the drain or the bounded batch left work behind.
    pub(crate) fn rearm_after_drain(&self) -> bool {
        self.notification_armed.store(false, Ordering::Release);
        if !self
            .pending_words
            .iter()
            .any(|word| word.load(Ordering::Acquire) != 0)
        {
            return false;
        }
        !self.notification_armed.swap(true, Ordering::AcqRel)
    }

    /// Recovers the doorbell after a failed wake without losing a racing
    /// producer's publication.
    ///
    /// `true` grants the caller one bounded wake retry. `false` means either no
    /// work remains or a racing producer already reclaimed the doorbell and
    /// performed its own wake.
    pub(crate) fn retry_after_failed_wake(&self) -> bool {
        self.retry_after_failed_wake_with(|| {})
    }

    fn retry_after_failed_wake_with(&self, after_clear: impl FnOnce()) -> bool {
        self.notification_armed.store(false, Ordering::Release);
        after_clear();
        core::sync::atomic::fence(Ordering::Acquire);
        if !self
            .pending_words
            .iter()
            .any(|word| word.load(Ordering::Acquire) != 0)
        {
            return false;
        }
        !self.notification_armed.swap(true, Ordering::AcqRel)
    }

    /// Permits one owner-run retry for a vPLIC pending/active collision.
    pub(crate) fn begin_collision_retry(&self, source: usize) -> bool {
        self.collision_retries.get(source).is_some_and(|retry| {
            retry
                .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    }

    pub(crate) fn clear_collision_retry(&self, source: usize) {
        if let Some(retry) = self.collision_retries.get(source) {
            retry.store(0, Ordering::Release);
        }
    }

    pub(crate) fn take_claim(&self, source: usize) -> u64 {
        self.claims
            .get(source)
            .map_or(0, |claim| claim.swap(0, Ordering::AcqRel))
    }

    /// Discards one stopped-route source after platform publishers drain.
    pub(crate) fn discard_for_revocation(&self, source: usize) -> u64 {
        if source == 0 || source >= self.claims.len() {
            self.record_fault();
            return 0;
        }
        let (word, bit) = pending_location(source);
        self.pending_words[word].fetch_and(!bit, Ordering::AcqRel);
        self.collision_retries[source].store(0, Ordering::Release);
        self.claims[source].swap(0, Ordering::AcqRel)
    }

    /// Closes the coalesced notification epoch after every source was removed.
    pub(crate) fn finish_revocation(&self) {
        self.notification_armed.store(false, Ordering::Release);
        for pending in &self.pending_words {
            if pending.swap(0, Ordering::AcqRel) != 0 {
                self.record_fault();
            }
        }
    }

    pub(crate) fn restore_claim(&self, source: usize, claim: u64) -> bool {
        self.claims.get(source).is_some_and(|slot| {
            slot.compare_exchange(0, claim, Ordering::Release, Ordering::Relaxed)
                .is_ok()
        })
    }

    pub(crate) fn record_fault(&self) {
        self.faults.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn fault_count(&self) -> usize {
        self.faults.load(Ordering::Relaxed)
    }
}

const fn pending_location(source: usize) -> (usize, u64) {
    (source / IRQ_WORD_BITS, 1u64 << (source % IRQ_WORD_BITS))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        alloc::{GlobalAlloc, Layout},
        cell::Cell,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use std::alloc::System;

    use super::*;

    #[global_allocator]
    static ALLOCATOR: AuditAllocator = AuditAllocator;
    static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static LOGS: AtomicUsize = AtomicUsize::new(0);

    std::thread_local! {
        static AUDIT_ENABLED: Cell<bool> = const { Cell::new(false) };
    }

    struct AuditAllocator;

    // SAFETY: all operations delegate to `System` with unchanged arguments;
    // the counters are observational and gated to the current test thread.
    unsafe impl GlobalAlloc for AuditAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() && audit_enabled() {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            pointer
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc_zeroed(layout) };
            if !pointer.is_null() && audit_enabled() {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            if audit_enabled() {
                DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            unsafe { System.dealloc(pointer, layout) };
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let replacement = unsafe { System.realloc(pointer, layout, new_size) };
            if !replacement.is_null() && audit_enabled() {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
                DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            replacement
        }
    }

    struct AuditLogger;

    impl log::Log for AuditLogger {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, _: &log::Record<'_>) {
            if audit_enabled() {
                LOGS.fetch_add(1, Ordering::Relaxed);
            }
        }

        fn flush(&self) {}
    }

    static LOGGER: AuditLogger = AuditLogger;

    #[test]
    fn hard_irq_publish_is_alloc_free_log_free_and_coalesced() {
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Trace);
        let ingress = ForwardedIrqIngress::new(128);

        let (first, allocations, deallocations, logs) =
            audit(|| ingress.publish(10, 0x1_0000_000a));
        assert_eq!(first, ForwardedIrqPublish::WakeOwner);
        assert_eq!((allocations, deallocations, logs), (0, 0, 0));

        let (second, allocations, deallocations, logs) =
            audit(|| ingress.publish(11, 0x1_0000_000b));
        assert_eq!(second, ForwardedIrqPublish::Coalesced);
        assert_eq!((allocations, deallocations, logs), (0, 0, 0));

        let faults = ingress.fault_count();
        assert_eq!(
            ingress.publish(10, 0x2_0000_000a),
            ForwardedIrqPublish::Fault
        );
        assert_eq!(ingress.fault_count(), faults + 1);
    }

    #[test]
    fn owner_drain_is_bounded_and_rearms_remainder() {
        let ingress = ForwardedIrqIngress::new(130);
        for source in 1..=65 {
            let result = ingress.publish(source, (1u64 << 32) | source as u64);
            assert_eq!(
                result,
                if source == 1 {
                    ForwardedIrqPublish::WakeOwner
                } else {
                    ForwardedIrqPublish::Coalesced
                }
            );
        }

        let first = ingress.take_batch();
        assert_eq!(first.entries().len(), FORWARDED_IRQ_DRAIN_BATCH);
        assert!(ingress.rearm_after_drain());
        let second = ingress.take_batch();
        assert_eq!(second.entries().len(), 1);
        assert_eq!(second.entries()[0].source(), 65);
        assert!(!ingress.rearm_after_drain());
    }

    #[test]
    fn failed_wake_recovery_hands_racing_publication_exactly_one_doorbell() {
        let ingress = ForwardedIrqIngress::new(128);
        assert_eq!(
            ingress.publish(10, 0x1_0000_000a),
            ForwardedIrqPublish::WakeOwner
        );
        let racing_owner = Cell::new(None);

        let retry_owner = ingress.retry_after_failed_wake_with(|| {
            racing_owner.set(Some(ingress.publish(11, 0x1_0000_000b)));
        });

        assert!(!retry_owner);
        assert_eq!(racing_owner.get(), Some(ForwardedIrqPublish::WakeOwner));
        let batch = ingress.take_batch();
        assert_eq!(batch.entries().len(), 2);
        assert!(!ingress.rearm_after_drain());
    }

    #[test]
    fn claim_lifecycle_and_collision_retry_are_bounded() {
        let ingress = ForwardedIrqIngress::new(128);
        let generation = 7;
        assert_eq!(
            ingress.publish(10, generation),
            ForwardedIrqPublish::WakeOwner
        );
        let batch = ingress.take_batch();
        assert_eq!(batch.entries()[0].claim(), generation);

        assert!(ingress.begin_collision_retry(10));
        assert!(!ingress.begin_collision_retry(10));
        ingress.clear_collision_retry(10);
        assert!(ingress.begin_collision_retry(10));

        ingress.requeue(10);
        assert!(ingress.rearm_after_drain());
        assert_eq!(ingress.take_claim(10), generation);
        assert!(ingress.restore_claim(10, generation));
        assert_eq!(ingress.take_claim(10), generation);

        // Exercise the no-race recovery wrapper: the retained pending bit is
        // reclaimed by the failed producer for one bounded wake retry.
        assert!(ingress.retry_after_failed_wake());
    }

    #[test]
    fn route_revocation_discards_claim_and_doorbell_without_republication() {
        let ingress = ForwardedIrqIngress::new(128);
        assert_eq!(ingress.publish(17, 9), ForwardedIrqPublish::WakeOwner);
        assert_eq!(ingress.discard_for_revocation(17), 9);
        ingress.finish_revocation();
        assert!(ingress.take_batch().entries().is_empty());
        assert_eq!(ingress.take_claim(17), 0);
    }

    fn audit<T>(operation: impl FnOnce() -> T) -> (T, usize, usize, usize) {
        AUDIT_ENABLED.with(|enabled| {
            ALLOCATIONS.store(0, Ordering::Relaxed);
            DEALLOCATIONS.store(0, Ordering::Relaxed);
            LOGS.store(0, Ordering::Relaxed);
            enabled.set(true);
            let value = operation();
            enabled.set(false);
            (
                value,
                ALLOCATIONS.load(Ordering::Relaxed),
                DEALLOCATIONS.load(Ordering::Relaxed),
                LOGS.load(Ordering::Relaxed),
            )
        })
    }

    fn audit_enabled() -> bool {
        AUDIT_ENABLED.try_with(Cell::get).unwrap_or(false)
    }
}
