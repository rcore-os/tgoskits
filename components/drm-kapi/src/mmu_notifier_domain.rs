// SPDX-License-Identifier: MIT
// Linux-compatible interval notifier sequencing used by DRM userptr memory.
//! Per-address-space MMU-notifier domains.
//!
//! This is the mechanism behind Linux `mm/mmu_notifier.c` interval
//! subscriptions. Each address space owns a [`Domain`]. A subscriber covers a
//! VA interval and is told, through [`Subscriber::invalidate`], before any
//! PTE covering that interval changes (or is torn down). The core memory
//! manager brackets each change with [`invalidate_range_start`], whose
//! [`InvalidationScope`] ends the invalidation when dropped, and [`release`]
//! runs when the address space is destroyed.
//!
//! Sequencing follows Linux exactly:
//!
//! * `Domain::seq` is `subscriptions->invalidate_seq`. It is odd while any
//!   invalidation is active and even when idle. A subscription's stored
//!   sequence starts at `seq - 1` (odd, never equal to the idle value).
//! * [`invalidate_range_start`] increments `active`, and if a subscriber
//!   overlaps the range, sets `seq |= 1` and calls `invalidate(seq)` on every
//!   overlapping subscriber. Each subscriber stores that odd value.
//! * The last [`InvalidationScope`] drop makes `seq` even again.
//! * [`read_begin`] samples a subscriber's stored sequence. If the subscriber
//!   is currently being invalidated, it waits for the domain sequence to move.
//!   [`read_retry`] reports whether the subscriber's sequence changed since the
//!   sample, which means the reader's pinned pages may be stale.
//!
//! Subscriptions and removals wait for active invalidations to finish, as the
//! Linux deferred-insert path does, so the subscriber set is stable while a
//! range is being invalidated.
extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;

/// Linux `enum mmu_notifier_event` values that the hosting kernel's mm raises.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// `MMU_NOTIFY_UNMAP`: `munmap`, `mremap` source, and other PTE teardown.
    Unmap          = 0,
    /// `MMU_NOTIFY_CLEAR`: zap/discard (`MADV_DONTNEED`, `MADV_FREE`) and
    /// reclaim of a resident page.
    Clear          = 1,
    /// `MMU_NOTIFY_PROTECTION_VMA`: `mprotect` / `pkey_mprotect`.
    ProtectionVma  = 2,
    /// `MMU_NOTIFY_PROTECTION_PAGE`: write-protection of a parent's PTEs at
    /// `fork` (copy-on-write setup).
    ProtectionPage = 3,
    /// `MMU_NOTIFY_RELEASE`: the address space is being destroyed.
    Release        = 5,
}

/// The invalidated interval `[start, end)` of one address space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub aspace: u64,
    pub start: usize,
    pub end: usize,
    pub event: Event,
}

/// One interval subscriber (Linux `struct mmu_interval_notifier` + ops).
pub trait Subscriber: Send + Sync {
    /// Linux `ops->invalidate`. Must store `seq` (odd) in the subscriber's
    /// read-side sequence before returning. The return value follows Linux:
    /// `false` means the subscriber could not invalidate without blocking.
    fn invalidate(&self, range: &Range, seq: u64) -> bool;
    /// The subscriber's stored sequence (`interval_sub->invalidate_seq`).
    fn seq(&self) -> u64;
    /// Stores the sequence at subscription time.
    fn set_seq(&self, seq: u64);
}

/// Why a subscription was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscribeError {
    /// The interval is empty or inverted.
    Invalid,
    /// `token` is already subscribed in this address space.
    Duplicate,
}

struct Entry {
    token: usize,
    first: usize,
    /// Inclusive, as in Linux `interval_tree` nodes.
    last: usize,
    sub: Arc<dyn Subscriber>,
}

struct State {
    /// `active_invalidate_ranges`.
    active: usize,
    entries: Vec<Entry>,
}

struct Domain {
    /// `subscriptions->invalidate_seq`. Writes happen under `state`; reads are
    /// lock-free for `read_begin`'s wait.
    seq: AtomicU64,
    state: Mutex<State>,
}

impl Domain {
    fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            state: Mutex::new(State {
                active: 0,
                entries: Vec::new(),
            }),
        }
    }
}

static DOMAINS: Mutex<BTreeMap<u64, Arc<Domain>>> = Mutex::new(BTreeMap::new());

fn domain(aspace: u64) -> Option<Arc<Domain>> {
    DOMAINS.lock().get(&aspace).cloned()
}

fn domain_or_create(aspace: u64) -> Arc<Domain> {
    DOMAINS
        .lock()
        .entry(aspace)
        .or_insert_with(|| Arc::new(Domain::new()))
        .clone()
}

/// Subscribes `sub` to invalidations of the inclusive interval
/// `[first, last]` in `aspace`. `token` identifies the subscription for
/// [`unsubscribe`]; it is normally the address of the subscriber's record.
pub fn subscribe(
    aspace: u64,
    token: usize,
    first: usize,
    last: usize,
    sub: Arc<dyn Subscriber>,
) -> Result<(), SubscribeError> {
    if first > last {
        return Err(SubscribeError::Invalid);
    }
    let domain = domain_or_create(aspace);
    loop {
        let mut state = domain.state.lock();
        if state.active != 0 {
            // A range is being invalidated; the subscriber set must not change
            // under its walk. Linux defers the insert instead of waiting.
            drop(state);
            core::hint::spin_loop();
            continue;
        }
        if state.entries.iter().any(|entry| entry.token == token) {
            return Err(SubscribeError::Duplicate);
        }
        // Idle: the initial sequence is odd and differs from the idle domain
        // value, so the first read_begin does not wait.
        sub.set_seq(domain.seq.load(Ordering::Acquire).wrapping_sub(1));
        state.entries.push(Entry {
            token,
            first,
            last,
            sub,
        });
        return Ok(());
    }
}

/// Removes the subscription identified by `token`. Waits for any active
/// invalidation to finish, as Linux `mmu_interval_notifier_remove()` does.
pub fn unsubscribe(aspace: u64, token: usize) {
    let Some(domain) = domain(aspace) else {
        return;
    };
    loop {
        let mut state = domain.state.lock();
        if state.active != 0 {
            drop(state);
            core::hint::spin_loop();
            continue;
        }
        state.entries.retain(|entry| entry.token != token);
        return;
    }
}

/// Begins a reader's critical section against `sub`. Returns the sequence to
/// pass to [`read_retry`]. If `sub` is being invalidated right now, this waits
/// until that invalidation ends (`mmu_interval_read_begin`).
pub fn read_begin(aspace: u64, sub: &dyn Subscriber) -> u64 {
    let Some(domain) = domain(aspace) else {
        return sub.seq();
    };
    let (seq, invalidating) = {
        let _state = domain.state.lock();
        let seq = sub.seq();
        (seq, seq == domain.seq.load(Ordering::Acquire))
    };
    if invalidating {
        while domain.seq.load(Ordering::Acquire) == seq {
            core::hint::spin_loop();
        }
    }
    seq
}

/// Ends a reader's critical section (`mmu_interval_read_retry`). `true` means
/// an invalidation collided with the section and the reader must retry.
pub fn read_retry(sub: &dyn Subscriber, seq: u64) -> bool {
    sub.seq() != seq
}

/// Holds an active invalidation of one range. Dropping it ends the
/// invalidation, so PTE changes made while it is held are bracketed by it.
#[must_use = "dropping the scope ends the invalidation immediately"]
pub struct InvalidationScope {
    domain: Option<Arc<Domain>>,
}

impl Drop for InvalidationScope {
    fn drop(&mut self) {
        let Some(domain) = self.domain.take() else {
            return;
        };
        let mut state = domain.state.lock();
        state.active -= 1;
        if state.active == 0 && domain.seq.load(Ordering::Acquire) & 1 == 1 {
            // mn_itree_inv_end(): make the sequence even again.
            domain.seq.fetch_add(1, Ordering::Release);
        }
    }
}

/// Linux `mmu_notifier_invalidate_range_start()`. Every subscriber overlapping
/// `[start, end)` is invalidated before this returns. Keep the returned scope
/// alive until after the PTE change it brackets.
pub fn invalidate_range_start(
    aspace: u64,
    start: usize,
    end: usize,
    event: Event,
) -> InvalidationScope {
    let Some(domain) = domain(aspace) else {
        return InvalidationScope { domain: None };
    };
    if start >= end {
        return InvalidationScope { domain: None };
    }
    let (cur_seq, matched) = {
        let mut state = domain.state.lock();
        state.active += 1;
        let matched: Vec<Arc<dyn Subscriber>> = state
            .entries
            .iter()
            .filter(|entry| entry.first < end && start <= entry.last)
            .map(|entry| entry.sub.clone())
            .collect();
        let cur_seq = if matched.is_empty() {
            0
        } else {
            let seq = domain.seq.load(Ordering::Acquire) | 1;
            domain.seq.store(seq, Ordering::Release);
            seq
        };
        (cur_seq, matched)
    };
    let scope = InvalidationScope {
        domain: Some(domain),
    };
    let range = Range {
        aspace,
        start,
        end,
        event,
    };
    for sub in matched {
        sub.invalidate(&range, cur_seq);
    }
    scope
}

/// Linux `mmu_notifier_release()`: the address space is being destroyed. Every
/// subscriber is invalidated over the whole address range with
/// [`Event::Release`], and the domain is dropped.
pub fn release(aspace: u64) {
    drop(invalidate_range_start(
        aspace,
        0,
        usize::MAX,
        Event::Release,
    ));
    DOMAINS.lock().remove(&aspace);
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicU32, AtomicUsize};

    use super::*;

    struct Recorder {
        seq: AtomicU64,
        hits: AtomicUsize,
        last_event: AtomicU32,
    }

    impl Recorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seq: AtomicU64::new(0),
                hits: AtomicUsize::new(0),
                last_event: AtomicU32::new(u32::MAX),
            })
        }
    }

    impl Subscriber for Recorder {
        fn invalidate(&self, range: &Range, seq: u64) -> bool {
            assert_eq!(seq & 1, 1, "invalidation must publish an odd sequence");
            self.seq.store(seq, Ordering::Release);
            self.last_event.store(range.event as u32, Ordering::Release);
            self.hits.fetch_add(1, Ordering::AcqRel);
            true
        }
        fn seq(&self) -> u64 {
            self.seq.load(Ordering::Acquire)
        }
        fn set_seq(&self, seq: u64) {
            self.seq.store(seq, Ordering::Release);
        }
    }

    const PAGE: usize = 0x1000;

    fn sub_over(aspace: u64, token: usize, first: usize, last: usize) -> Arc<Recorder> {
        let rec = Recorder::new();
        subscribe(aspace, token, first, last, rec.clone()).unwrap();
        rec
    }

    #[test]
    fn munmap_invalidates_overlapping_interval_only() {
        let aspace = 0x1001;
        // Subscriber covers pages [0x1000, 0x2fff].
        let rec = sub_over(aspace, 1, PAGE, 3 * PAGE - 1);
        // Overlapping unmap of [0x2000, 0x3000) fires.
        drop(invalidate_range_start(
            aspace,
            2 * PAGE,
            3 * PAGE,
            Event::Unmap,
        ));
        assert_eq!(rec.hits.load(Ordering::Acquire), 1);
        assert_eq!(rec.last_event.load(Ordering::Acquire), Event::Unmap as u32);
        // Disjoint unmap of [0x3000, 0x4000) and the touching-below [0, 0x1000)
        // do not fire: intervals are half-open on the range side.
        drop(invalidate_range_start(
            aspace,
            3 * PAGE,
            4 * PAGE,
            Event::Unmap,
        ));
        drop(invalidate_range_start(aspace, 0, PAGE, Event::Unmap));
        assert_eq!(rec.hits.load(Ordering::Acquire), 1);
        unsubscribe(aspace, 1);
    }

    #[test]
    fn mprotect_and_madvise_discard_invalidate_with_their_events() {
        let aspace = 0x1002;
        let rec = sub_over(aspace, 1, 0x10_0000, 0x10_0000 + 4 * PAGE - 1);
        drop(invalidate_range_start(
            aspace,
            0x10_0000 + PAGE,
            0x10_0000 + 2 * PAGE,
            Event::ProtectionVma,
        ));
        assert_eq!(rec.hits.load(Ordering::Acquire), 1);
        assert_eq!(
            rec.last_event.load(Ordering::Acquire),
            Event::ProtectionVma as u32
        );
        // MADV_DONTNEED zaps through the Clear event.
        drop(invalidate_range_start(
            aspace,
            0x10_0000,
            0x10_0000 + PAGE,
            Event::Clear,
        ));
        assert_eq!(rec.hits.load(Ordering::Acquire), 2);
        assert_eq!(rec.last_event.load(Ordering::Acquire), Event::Clear as u32);
        // A non-overlapping madvise range leaves the subscriber alone.
        drop(invalidate_range_start(
            aspace,
            0x20_0000,
            0x20_0000 + PAGE,
            Event::Clear,
        ));
        assert_eq!(rec.hits.load(Ordering::Acquire), 2);
        unsubscribe(aspace, 1);
    }

    #[test]
    fn read_retry_observes_invalidation_and_not_idle_time() {
        let aspace = 0x1003;
        let rec = sub_over(aspace, 1, 0, 8 * PAGE);
        let seq = read_begin(aspace, rec.as_ref());
        // Nothing invalidated since the sample: no retry.
        assert!(!read_retry(rec.as_ref(), seq));
        drop(invalidate_range_start(aspace, PAGE, 2 * PAGE, Event::Unmap));
        // The invalidation changed the subscriber's sequence: retry.
        assert!(read_retry(rec.as_ref(), seq));
        // A fresh sample after the invalidation ended is stable again.
        let fresh = read_begin(aspace, rec.as_ref());
        assert_ne!(fresh, seq);
        assert!(!read_retry(rec.as_ref(), fresh));
        unsubscribe(aspace, 1);
    }

    #[test]
    fn release_invalidates_everything_and_drops_the_domain() {
        let aspace = 0x1004;
        let high = sub_over(aspace, 1, 0x7f00_0000_0000, 0x7f00_0000_0000 + PAGE);
        let low = sub_over(aspace, 2, PAGE, 2 * PAGE);
        release(aspace);
        assert_eq!(high.hits.load(Ordering::Acquire), 1);
        assert_eq!(low.hits.load(Ordering::Acquire), 1);
        assert_eq!(
            high.last_event.load(Ordering::Acquire),
            Event::Release as u32
        );
        // The domain is gone: later invalidations deliver nothing.
        drop(invalidate_range_start(aspace, PAGE, 2 * PAGE, Event::Unmap));
        assert_eq!(low.hits.load(Ordering::Acquire), 1);
    }

    #[test]
    fn duplicate_and_inverted_subscriptions_are_refused() {
        let aspace = 0x1005;
        let rec = Recorder::new();
        assert_eq!(
            subscribe(aspace, 7, 10, 5, rec.clone()),
            Err(SubscribeError::Invalid)
        );
        subscribe(aspace, 7, 0, PAGE, rec.clone()).unwrap();
        assert_eq!(
            subscribe(aspace, 7, 0, PAGE, rec),
            Err(SubscribeError::Duplicate)
        );
        unsubscribe(aspace, 7);
    }

    #[test]
    fn unsubscribed_interval_is_no_longer_invalidated() {
        let aspace = 0x1006;
        let rec = sub_over(aspace, 1, 0, PAGE);
        unsubscribe(aspace, 1);
        drop(invalidate_range_start(aspace, 0, PAGE, Event::Unmap));
        assert_eq!(rec.hits.load(Ordering::Acquire), 0);
    }
}
