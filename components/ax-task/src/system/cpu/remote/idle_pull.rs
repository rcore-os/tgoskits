use super::*;

const IDLE_PULL_PHASE_MASK: u64 = 0b11;
const IDLE_PULL_IDLE: u64 = 0;
pub(super) const INITIAL_IDLE_PULL_STATE: u64 = IDLE_PULL_IDLE;
const IDLE_PULL_PENDING: u64 = 1;
const IDLE_PULL_CLAIMED: u64 = 2;
const IDLE_PULL_COMMITTED: u64 = 3;
const IDLE_PULL_PUBLISHER_SHIFT: u32 = 2;
const IDLE_PULL_PUBLISHER_BITS: u32 = 16;
const IDLE_PULL_PUBLISHER_ONE: u64 = 1 << IDLE_PULL_PUBLISHER_SHIFT;
const IDLE_PULL_PUBLISHER_MASK: u64 =
    ((1 << IDLE_PULL_PUBLISHER_BITS) - 1) << IDLE_PULL_PUBLISHER_SHIFT;
const IDLE_PULL_GENERATION_STEP: u64 = 1 << (IDLE_PULL_PUBLISHER_SHIFT + IDLE_PULL_PUBLISHER_BITS);
const IDLE_PULL_GENERATION_MASK: u64 = !(IDLE_PULL_PHASE_MASK | IDLE_PULL_PUBLISHER_MASK);
const IDLE_PULL_PUBLISHER_OVERFLOW_INVARIANT: u32 = 0x4944_4c50;

#[derive(Debug)]
pub(super) struct IdlePullState {
    state: AtomicU64,
}

impl IdlePullState {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU64::new(INITIAL_IDLE_PULL_STATE),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdlePullReservation {
    Started(u64),
    AlreadyPending,
    Busy,
}

pub(crate) struct IdlePullClaim<'remote> {
    remote: &'remote CpuRemote,
    state: u64,
}

impl IdlePullClaim<'_> {
    /// Linearizes the pull before the target admits newer runnable work.
    pub(crate) fn commit(&mut self) -> bool {
        let committed = (self.state & !IDLE_PULL_PHASE_MASK) | IDLE_PULL_COMMITTED;
        if self
            .remote
            .idle_pull
            .state
            .compare_exchange(self.state, committed, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        self.state = committed;
        true
    }
}

impl Drop for IdlePullClaim<'_> {
    fn drop(&mut self) {
        let generation = self.state & IDLE_PULL_GENERATION_MASK;
        let phase = self.state & IDLE_PULL_PHASE_MASK;
        let mut current = self.remote.idle_pull.state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_GENERATION_MASK != generation
                || current & IDLE_PULL_PHASE_MASK != phase
            {
                return;
            }
            let idle = current & !IDLE_PULL_PHASE_MASK;
            match self.remote.idle_pull.state.compare_exchange_weak(
                current,
                idle,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }
}

pub(super) struct IdlePullWorkPublication<'remote> {
    remote: &'remote CpuRemote,
}

impl Drop for IdlePullWorkPublication<'_> {
    fn drop(&mut self) {
        let previous = self
            .remote
            .idle_pull
            .state
            .fetch_sub(IDLE_PULL_PUBLISHER_ONE, Ordering::Release);
        debug_assert_ne!(
            previous & IDLE_PULL_PUBLISHER_MASK,
            0,
            "idle-pull work publisher count underflowed"
        );
    }
}

impl CpuRemote {
    pub(crate) fn begin_idle_pull(&self) -> IdlePullReservation {
        let mut current = self.idle_pull.state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_PUBLISHER_MASK != 0 {
                return IdlePullReservation::Busy;
            }
            if current & IDLE_PULL_PHASE_MASK != IDLE_PULL_IDLE {
                return IdlePullReservation::AlreadyPending;
            }
            let generation = (current & IDLE_PULL_GENERATION_MASK)
                .wrapping_add(IDLE_PULL_GENERATION_STEP)
                & IDLE_PULL_GENERATION_MASK;
            let pending = generation | IDLE_PULL_PENDING;
            match self.idle_pull.state.compare_exchange_weak(
                current,
                pending,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return IdlePullReservation::Started(pending),
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn cancel_idle_pull(&self, reservation: u64) {
        let generation = reservation & IDLE_PULL_GENERATION_MASK;
        let mut current = self.idle_pull.state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_GENERATION_MASK != generation
                || !matches!(
                    current & IDLE_PULL_PHASE_MASK,
                    IDLE_PULL_PENDING | IDLE_PULL_CLAIMED
                )
            {
                return;
            }
            let idle = current & !IDLE_PULL_PHASE_MASK;
            match self.idle_pull.state.compare_exchange_weak(
                current,
                idle,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn cancel_idle_pull_if_uncommitted(&self) {
        let mut current = self.idle_pull.state.load(Ordering::Acquire);
        loop {
            match current & IDLE_PULL_PHASE_MASK {
                IDLE_PULL_IDLE | IDLE_PULL_COMMITTED => return,
                IDLE_PULL_PENDING | IDLE_PULL_CLAIMED => {}
                _ => unreachable!(),
            }
            let idle = current & !IDLE_PULL_PHASE_MASK;
            match self.idle_pull.state.compare_exchange_weak(
                current,
                idle,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    pub(super) fn begin_idle_pull_work(&self) -> IdlePullWorkPublication<'_> {
        let mut current = self.idle_pull.state.load(Ordering::Acquire);
        loop {
            if current & IDLE_PULL_PUBLISHER_MASK == IDLE_PULL_PUBLISHER_MASK {
                task_runtime::fatal_invariant(
                    IDLE_PULL_PUBLISHER_OVERFLOW_INVARIANT,
                    self.owner.as_u32() as usize,
                );
            }
            let phase = match current & IDLE_PULL_PHASE_MASK {
                IDLE_PULL_PENDING | IDLE_PULL_CLAIMED => IDLE_PULL_IDLE,
                phase => phase,
            };
            let next = ((current + IDLE_PULL_PUBLISHER_ONE) & !IDLE_PULL_PHASE_MASK) | phase;
            match self.idle_pull.state.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return IdlePullWorkPublication { remote: self },
                Err(actual) => current = actual,
            }
        }
    }

    pub(crate) fn claim_idle_pull(&self, reservation: u64) -> Option<IdlePullClaim<'_>> {
        if reservation & (IDLE_PULL_PHASE_MASK | IDLE_PULL_PUBLISHER_MASK) != IDLE_PULL_PENDING {
            return None;
        }
        let claimed = (reservation & !IDLE_PULL_PHASE_MASK) | IDLE_PULL_CLAIMED;
        self.idle_pull
            .state
            .compare_exchange(reservation, claimed, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| IdlePullClaim {
                remote: self,
                state: claimed,
            })
    }

    pub(super) fn idle_pull_is_quiescent(&self) -> bool {
        self.idle_pull.state.load(Ordering::Acquire)
            & (IDLE_PULL_PHASE_MASK | IDLE_PULL_PUBLISHER_MASK)
            == IDLE_PULL_IDLE
    }
}
