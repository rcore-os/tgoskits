//! Lock-free publication state shared by a LoongArch passthrough IRQ action
//! and its task-context owner.

use core::sync::atomic::{AtomicU64, Ordering};

const NO_GENERATION: u64 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CapturePublication {
    Published,
    Coalesced,
    Stale,
}

pub(super) struct RoutePublication {
    next_generation: AtomicU64,
    active_generation: AtomicU64,
    pending_generation: AtomicU64,
    quench_generation: AtomicU64,
    rearm_request_generation: AtomicU64,
}

impl RoutePublication {
    pub(super) const fn new() -> Self {
        Self {
            next_generation: AtomicU64::new(NO_GENERATION),
            active_generation: AtomicU64::new(NO_GENERATION),
            pending_generation: AtomicU64::new(NO_GENERATION),
            quench_generation: AtomicU64::new(NO_GENERATION),
            rearm_request_generation: AtomicU64::new(NO_GENERATION),
        }
    }

    pub(super) fn allocate_generation(&self) -> Option<u64> {
        self.next_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation
                    .checked_add(1)
                    .filter(|next| *next != NO_GENERATION)
            })
            .ok()
            .and_then(|generation| generation.checked_add(1))
    }

    pub(super) fn activate(&self, generation: u64) {
        assert_ne!(generation, NO_GENERATION);
        assert_eq!(
            self.pending_generation.load(Ordering::Acquire),
            NO_GENERATION,
            "LoongArch route activation retained a pending older generation"
        );
        assert_eq!(
            self.quench_generation.load(Ordering::Acquire),
            NO_GENERATION,
            "LoongArch route activation retained an older line quench"
        );
        assert_eq!(
            self.rearm_request_generation.load(Ordering::Acquire),
            NO_GENERATION,
            "LoongArch route activation retained an older rearm request"
        );
        self.active_generation.store(generation, Ordering::Release);
    }

    pub(super) fn deactivate(&self, generation: u64) {
        let _ = self.active_generation.compare_exchange(
            generation,
            NO_GENERATION,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(super) fn capture(&self, generation: u64) -> CapturePublication {
        let active = self.active_generation.load(Ordering::Acquire) == generation;
        let quench_matches = publish_generation(&self.quench_generation, generation);
        if !active || !quench_matches {
            return CapturePublication::Stale;
        }
        match self.pending_generation.compare_exchange(
            NO_GENERATION,
            generation,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => CapturePublication::Published,
            Err(observed) if observed == generation => CapturePublication::Coalesced,
            Err(_) => CapturePublication::Stale,
        }
    }

    pub(super) fn take_pending(&self, generation: u64) -> bool {
        self.pending_generation
            .compare_exchange(
                generation,
                NO_GENERATION,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn restore_pending(&self, generation: u64) {
        let published = publish_generation(&self.pending_generation, generation);
        assert!(
            published,
            "LoongArch route could not restore its exact pending generation"
        );
    }

    pub(super) fn begin_rearm(&self, generation: u64) -> bool {
        self.quench_generation
            .compare_exchange(
                generation,
                NO_GENERATION,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn request_rearm(&self, generation: u64) -> bool {
        self.active_generation.load(Ordering::Acquire) == generation
            && publish_generation(&self.rearm_request_generation, generation)
    }

    pub(super) fn take_rearm_request(&self, generation: u64) -> bool {
        self.rearm_request_generation
            .compare_exchange(
                generation,
                NO_GENERATION,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn restore_rearm_request(&self, generation: u64) {
        let published = publish_generation(&self.rearm_request_generation, generation);
        assert!(
            published,
            "LoongArch route could not restore its exact rearm request"
        );
    }

    pub(super) fn restore_quench(&self, generation: u64) {
        let published = publish_generation(&self.quench_generation, generation);
        assert!(
            published,
            "LoongArch route could not restore its exact quench generation"
        );
    }

    pub(super) fn clear_after_release(&self, generation: u64) {
        self.deactivate(generation);
        let _ = self.pending_generation.compare_exchange(
            generation,
            NO_GENERATION,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        let _ = self.rearm_request_generation.compare_exchange(
            generation,
            NO_GENERATION,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        assert_eq!(
            self.quench_generation.load(Ordering::Acquire),
            NO_GENERATION,
            "LoongArch route released its action while a line quench remained owned"
        );
    }
}

fn publish_generation(target: &AtomicU64, generation: u64) -> bool {
    match target.compare_exchange(
        NO_GENERATION,
        generation,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => true,
        Err(observed) => observed == generation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_capture_coalesces_and_retains_one_quench_owner() {
        let publication = RoutePublication::new();
        let generation = publication.allocate_generation().unwrap();
        publication.activate(generation);

        assert_eq!(
            publication.capture(generation),
            CapturePublication::Published
        );
        assert_eq!(
            publication.capture(generation),
            CapturePublication::Coalesced
        );
        assert!(publication.take_pending(generation));
        assert!(!publication.take_pending(generation));
        publication.restore_pending(generation);
        assert!(publication.take_pending(generation));
        assert!(publication.request_rearm(generation));
        assert!(publication.take_rearm_request(generation));
        publication.restore_rearm_request(generation);
        assert!(publication.take_rearm_request(generation));
        assert!(publication.begin_rearm(generation));
        publication.restore_quench(generation);
        assert!(publication.begin_rearm(generation));
        publication.clear_after_release(generation);
    }

    #[test]
    fn stale_capture_is_contained_without_becoming_owner_work() {
        let publication = RoutePublication::new();
        let generation = publication.allocate_generation().unwrap();
        publication.activate(generation);
        publication.deactivate(generation);

        assert_eq!(publication.capture(generation), CapturePublication::Stale);
        assert!(!publication.take_pending(generation));
        assert!(!publication.request_rearm(generation));
        assert!(publication.begin_rearm(generation));
        publication.clear_after_release(generation);
    }

    #[test]
    fn old_eoi_cannot_release_a_new_generation() {
        let publication = RoutePublication::new();
        let old = publication.allocate_generation().unwrap();
        publication.activate(old);
        assert_eq!(publication.capture(old), CapturePublication::Published);
        assert!(publication.take_pending(old));
        assert!(publication.begin_rearm(old));
        publication.clear_after_release(old);

        let new = publication.allocate_generation().unwrap();
        publication.activate(new);
        assert_eq!(publication.capture(new), CapturePublication::Published);
        assert!(!publication.begin_rearm(old));
        assert!(publication.begin_rearm(new));
        publication.clear_after_release(new);
    }
}
