//! PREEMPT_RT-style per-CPU soft-timer worker publication.
//!
//! The hrtimer base remains the only owner of timeout payload. This state is
//! equivalent to Linux's per-CPU timer-softirq pending bit plus the
//! `ktimers/%u` smpboot thread: hard IRQ only publishes a sticky event, while
//! the fixed worker drains timeout wakeups in task context.

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use super::*;
use crate::IrqWaitCell;

const WORKER_UNINSTALLED: u8 = 0;
const WORKER_STARTING: u8 = 1;
const WORKER_INSTALLED: u8 = 2;

#[derive(Debug)]
pub(super) struct KtimerWorkerState {
    event: IrqWaitCell,
    worker_state: AtomicU8,
    worker_thread: AtomicU64,
    published_generation: AtomicU64,
    completed_generation: AtomicU64,
    in_service: AtomicBool,
}

#[derive(Debug)]
pub(crate) struct KtimerWorkClaim {
    generation: u64,
}

impl KtimerWorkerState {
    pub(super) const fn new() -> Self {
        Self {
            event: IrqWaitCell::new(),
            worker_state: AtomicU8::new(WORKER_UNINSTALLED),
            worker_thread: AtomicU64::new(0),
            published_generation: AtomicU64::new(0),
            completed_generation: AtomicU64::new(0),
            in_service: AtomicBool::new(false),
        }
    }

    fn begin_install(&self) -> Result<(), TaskError> {
        self.worker_state
            .compare_exchange(
                WORKER_UNINSTALLED,
                WORKER_STARTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| TaskError::InvalidConfiguration)
    }

    fn finish_install(&self, thread: ThreadId) {
        assert_ne!(
            thread.as_u64(),
            0,
            "ktimer worker must have a generation-bearing identity"
        );
        assert_eq!(
            self.worker_thread.compare_exchange(
                0,
                thread.as_u64(),
                Ordering::Release,
                Ordering::Acquire
            ),
            Ok(0),
            "ktimer worker identity was installed twice"
        );
        assert_eq!(
            self.worker_state.compare_exchange(
                WORKER_STARTING,
                WORKER_INSTALLED,
                Ordering::Release,
                Ordering::Acquire,
            ),
            Ok(WORKER_STARTING),
            "ktimer worker completed installation from an invalid state"
        );
    }

    fn cancel_install(&self) {
        assert_eq!(
            self.worker_state.compare_exchange(
                WORKER_STARTING,
                WORKER_UNINSTALLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ),
            Ok(WORKER_STARTING),
            "ktimer worker cancelled installation from an invalid state"
        );
    }

    fn worker_thread(&self) -> Option<ThreadId> {
        let raw = self.worker_thread.load(Ordering::Acquire);
        (raw != 0).then(|| ThreadId::from_parts(raw as u32, (raw >> 32) as u32))
    }

    fn is_quiescent_for_offline(&self) -> bool {
        let generations_complete = self.published_generation.load(Ordering::Acquire)
            == self.completed_generation.load(Ordering::Acquire);
        let service_idle = !self.in_service.load(Ordering::Acquire);
        match self.worker_state.load(Ordering::Acquire) {
            WORKER_UNINSTALLED | WORKER_INSTALLED => {
                generations_complete && service_idle && !self.event.is_pending()
            }
            WORKER_STARTING => false,
            state => task_runtime::fatal_invariant(0x4b54_0001, state as usize),
        }
    }

    const fn event(&self) -> &IrqWaitCell {
        &self.event
    }

    fn publish(&self) {
        self.published_generation
            .try_update(Ordering::Release, Ordering::Relaxed, |generation| {
                generation.checked_add(1)
            })
            .unwrap_or_else(|_| task_runtime::fatal_invariant(0x4b54_0002, usize::MAX));
        let _notified = self.event.notify();
    }

    fn claim(&self) -> Option<KtimerWorkClaim> {
        let published = self.published_generation.load(Ordering::Acquire);
        if published == self.completed_generation.load(Ordering::Acquire) {
            return None;
        }
        if self
            .in_service
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            task_runtime::fatal_invariant(0x4b54_0003, published as usize);
        }
        Some(KtimerWorkClaim {
            generation: published,
        })
    }

    fn complete(&self, claim: KtimerWorkClaim) {
        let previous = self
            .completed_generation
            .swap(claim.generation, Ordering::AcqRel);
        if previous > claim.generation {
            task_runtime::fatal_invariant(0x4b54_0004, previous as usize);
        }
        if !self.in_service.swap(false, Ordering::Release) {
            task_runtime::fatal_invariant(0x4b54_0005, claim.generation as usize);
        }
        if self.published_generation.load(Ordering::Acquire) != claim.generation {
            let _notified = self.event.notify();
        }
    }
}

impl CpuRemote {
    pub(crate) fn begin_ktimer_worker_install(&self) -> Result<(), TaskError> {
        self.ktimer.begin_install()
    }

    pub(crate) fn finish_ktimer_worker_install(&self, thread: ThreadId) {
        self.ktimer.finish_install(thread);
    }

    pub(crate) fn cancel_ktimer_worker_install(&self) {
        self.ktimer.cancel_install();
    }

    pub(crate) fn publish_ktimer_work(&self) {
        self.ktimer.publish();
    }

    pub(crate) fn claim_ktimer_work(&self) -> Option<KtimerWorkClaim> {
        self.ktimer.claim()
    }

    pub(crate) fn complete_ktimer_work(&self, claim: KtimerWorkClaim) {
        self.ktimer.complete(claim);
    }

    pub(crate) const fn ktimer_event(&self) -> &IrqWaitCell {
        self.ktimer.event()
    }

    pub(crate) fn ktimer_worker(&self) -> Option<ThreadId> {
        self.ktimer.worker_thread()
    }

    pub(in crate::system::cpu) fn ktimer_is_quiescent_for_offline(&self) -> bool {
        self.ktimer.is_quiescent_for_offline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_published_before_worker_install_remains_pending() {
        let state = KtimerWorkerState::new();
        let worker = ThreadId::from_parts(1, 1);

        state.publish();
        assert!(state.event().is_pending());
        assert!(
            !state.is_quiescent_for_offline(),
            "pending work must gate CPU offline before worker registration"
        );
        state.begin_install().unwrap();
        assert!(!state.is_quiescent_for_offline());
        state.finish_install(worker);

        assert_eq!(state.worker_thread(), Some(worker));
        assert!(
            state.event().is_pending(),
            "installing the worker must not consume work published before registration"
        );
        assert!(!state.is_quiescent_for_offline());
    }

    #[test]
    fn worker_install_state_gates_cpu_offline() {
        let state = KtimerWorkerState::new();
        assert!(state.is_quiescent_for_offline());

        state.begin_install().unwrap();
        assert!(!state.is_quiescent_for_offline());
        state.cancel_install();
        assert!(state.is_quiescent_for_offline());

        state.begin_install().unwrap();
        state.finish_install(ThreadId::from_parts(1, 1));
        assert!(state.is_quiescent_for_offline());
        state.publish();
        assert!(!state.is_quiescent_for_offline());
    }

    #[test]
    fn publication_during_service_remains_a_new_generation() {
        let state = KtimerWorkerState::new();
        state.begin_install().unwrap();
        state.finish_install(ThreadId::from_parts(1, 1));
        state.publish();

        let first = state.claim().expect("published work must be claimable");
        assert!(state.in_service.load(Ordering::Acquire));
        assert!(!state.is_quiescent_for_offline());
        state.publish();
        state.complete(first);

        assert_eq!(state.published_generation.load(Ordering::Acquire), 2);
        assert_eq!(state.completed_generation.load(Ordering::Acquire), 1);
        let second = state
            .claim()
            .expect("publication racing service must survive completion");
        state.complete(second);
        assert_eq!(state.completed_generation.load(Ordering::Acquire), 2);
        assert!(!state.in_service.load(Ordering::Acquire));
    }

    #[test]
    fn completed_generation_is_the_work_authority() {
        let state = KtimerWorkerState::new();
        assert!(state.claim().is_none());
        state.publish();
        state.publish();

        let claim = state
            .claim()
            .expect("coalesced doorbell must retain the latest generation");
        assert_eq!(claim.generation, 2);
        state.complete(claim);
        assert!(state.claim().is_none());
        assert_eq!(state.published_generation.load(Ordering::Acquire), 2);
        assert_eq!(state.completed_generation.load(Ordering::Acquire), 2);
    }
}
