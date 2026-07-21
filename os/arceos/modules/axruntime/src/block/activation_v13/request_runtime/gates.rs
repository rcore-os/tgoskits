//! Runtime-owned admission and hardware-dispatch lifecycle gates.

use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

const ADMISSION_STATE_SHIFT: u32 = usize::BITS - 2;
const ADMISSION_COUNT_MASK: usize = (1_usize << ADMISSION_STATE_SHIFT) - 1;
const ADMISSION_STATE_MASK: usize = !ADMISSION_COUNT_MASK;
const ADMISSION_OPEN: usize = 0;
const ADMISSION_FROZEN: usize = 1_usize << ADMISSION_STATE_SHIFT;
const ADMISSION_CLOSED: usize = 2_usize << ADMISSION_STATE_SHIFT;

/// Admission failure that preserves whether retry can ever succeed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum AdmissionError {
    #[error("block request admission is frozen")]
    Frozen,
    #[error("block request admission is permanently closed")]
    Closed,
    #[error("block request admission has too many concurrent submitters")]
    Saturated,
    #[error("block request admission must be frozen before it can close")]
    StillOpen,
    #[error("block request admission still has {0} active submitters")]
    ActiveSubmitters(usize),
}

/// Snapshot returned by the linearizing Open-to-Frozen transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::block::activation_v13) struct AdmissionFreezeProgress {
    active_submitters: usize,
}

impl AdmissionFreezeProgress {
    #[cfg(test)]
    pub(in crate::block::activation_v13) const fn active_submitters(self) -> usize {
        self.active_submitters
    }
}

/// Packed admission state and pre-freeze submitter count.
///
/// A submitter increments the low bits only while the high state bits are
/// Open. Freezing changes those state bits in the same compare-exchange, so a
/// racing submit either owns a counted permit or observes Frozen; it can never
/// become invisible to the drain owner.
pub(super) struct AdmissionGate {
    state_and_submitters: AtomicUsize,
}

impl AdmissionGate {
    pub(super) const fn new() -> Self {
        Self {
            state_and_submitters: AtomicUsize::new(ADMISSION_OPEN),
        }
    }

    pub(super) fn try_admit(&self) -> Result<AdmissionPermit<'_>, AdmissionError> {
        let mut observed = self.state_and_submitters.load(Ordering::Acquire);
        loop {
            match observed & ADMISSION_STATE_MASK {
                ADMISSION_OPEN => {}
                ADMISSION_FROZEN => return Err(AdmissionError::Frozen),
                ADMISSION_CLOSED => return Err(AdmissionError::Closed),
                _ => unreachable!("invalid block admission state"),
            }
            if observed & ADMISSION_COUNT_MASK == ADMISSION_COUNT_MASK {
                return Err(AdmissionError::Saturated);
            }
            match self.state_and_submitters.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(AdmissionPermit { gate: self }),
                Err(actual) => observed = actual,
            }
        }
    }

    pub(super) fn begin_freeze(&self) -> Result<AdmissionFreezeProgress, AdmissionError> {
        let mut observed = self.state_and_submitters.load(Ordering::Acquire);
        loop {
            match observed & ADMISSION_STATE_MASK {
                ADMISSION_OPEN => {
                    let frozen = (observed & ADMISSION_COUNT_MASK) | ADMISSION_FROZEN;
                    match self.state_and_submitters.compare_exchange_weak(
                        observed,
                        frozen,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => {
                            return Ok(AdmissionFreezeProgress {
                                active_submitters: frozen & ADMISSION_COUNT_MASK,
                            });
                        }
                        Err(actual) => observed = actual,
                    }
                }
                ADMISSION_FROZEN => {
                    return Ok(AdmissionFreezeProgress {
                        active_submitters: observed & ADMISSION_COUNT_MASK,
                    });
                }
                ADMISSION_CLOSED => return Err(AdmissionError::Closed),
                _ => unreachable!("invalid block admission state"),
            }
        }
    }

    pub(super) fn is_frozen_and_idle(&self) -> bool {
        self.state_and_submitters.load(Ordering::Acquire) == ADMISSION_FROZEN
    }

    pub(super) fn thaw(&self) -> Result<(), AdmissionError> {
        match self.state_and_submitters.load(Ordering::Acquire) {
            ADMISSION_OPEN => Ok(()),
            ADMISSION_FROZEN => self
                .state_and_submitters
                .compare_exchange(
                    ADMISSION_FROZEN,
                    ADMISSION_OPEN,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map(|_| ())
                .map_err(|actual| admission_transition_error(actual, AdmissionError::Frozen)),
            actual if actual & ADMISSION_STATE_MASK == ADMISSION_FROZEN => Err(
                AdmissionError::ActiveSubmitters(actual & ADMISSION_COUNT_MASK),
            ),
            ADMISSION_CLOSED => Err(AdmissionError::Closed),
            _ => unreachable!("invalid block admission state"),
        }
    }

    pub(super) fn close(&self) -> Result<(), AdmissionError> {
        match self.state_and_submitters.load(Ordering::Acquire) {
            ADMISSION_FROZEN => self
                .state_and_submitters
                .compare_exchange(
                    ADMISSION_FROZEN,
                    ADMISSION_CLOSED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .map(|_| ())
                .map_err(|actual| admission_transition_error(actual, AdmissionError::Frozen)),
            actual if actual & ADMISSION_STATE_MASK == ADMISSION_FROZEN => Err(
                AdmissionError::ActiveSubmitters(actual & ADMISSION_COUNT_MASK),
            ),
            actual if actual & ADMISSION_STATE_MASK == ADMISSION_OPEN => {
                Err(AdmissionError::StillOpen)
            }
            ADMISSION_CLOSED => Ok(()),
            _ => unreachable!("invalid block admission state"),
        }
    }
}

fn admission_transition_error(actual: usize, frozen_error: AdmissionError) -> AdmissionError {
    match actual & ADMISSION_STATE_MASK {
        ADMISSION_OPEN => AdmissionError::StillOpen,
        ADMISSION_FROZEN => {
            let active = actual & ADMISSION_COUNT_MASK;
            if active == 0 {
                frozen_error
            } else {
                AdmissionError::ActiveSubmitters(active)
            }
        }
        ADMISSION_CLOSED => AdmissionError::Closed,
        _ => unreachable!("invalid block admission state"),
    }
}

/// Counted right to finish one admission that linearized before freeze.
pub(super) struct AdmissionPermit<'gate> {
    gate: &'gate AdmissionGate,
}

impl Drop for AdmissionPermit<'_> {
    fn drop(&mut self) {
        let previous = self
            .gate
            .state_and_submitters
            .fetch_sub(1, Ordering::AcqRel);
        assert!(
            previous & ADMISSION_COUNT_MASK != 0,
            "block admission submitter count underflowed"
        );
    }
}

/// Owner-visible hardware dispatch phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(in crate::block::activation_v13) enum DispatchState {
    Running  = 0,
    Draining = 1,
    Quiesced = 2,
    Closed   = 3,
}

impl DispatchState {
    fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Running,
            1 => Self::Draining,
            2 => Self::Quiesced,
            3 => Self::Closed,
            _ => panic!("invalid block dispatch state {raw}"),
        }
    }
}

/// Dispatch lifecycle failure with the observed owner state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(in crate::block::activation_v13) enum DispatchGateError {
    #[error("block dispatch is permanently closed")]
    Closed,
    #[error("block dispatch transition requires {expected:?}, observed {actual:?}")]
    InvalidState {
        expected: DispatchState,
        actual: DispatchState,
    },
}

/// Proof that no request remains eligible for a new hardware submission.
pub(super) struct DispatchCutoffProof {
    _private: (),
}

impl DispatchCutoffProof {
    /// Creates a dispatch-cutoff proof after one stable owner-side observation.
    ///
    /// # Safety
    ///
    /// Admission must already be Frozen and idle. Every software context and
    /// owner dispatch list must be empty. The caller must remain the sole
    /// domain owner until this proof is consumed. Hardware-owned requests may
    /// remain InFlight and must be completed normally or reclaimed after DMA
    /// quiesce.
    pub(super) const unsafe fn new_unchecked() -> Self {
        Self { _private: () }
    }

    #[cfg(test)]
    const fn for_test() -> Self {
        Self { _private: () }
    }
}

/// Owner-local gate separating request drain from hardware quiescence.
pub(super) struct DispatchGate {
    state: AtomicU8,
}

impl DispatchGate {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(DispatchState::Running as u8),
        }
    }

    pub(super) fn state(&self) -> DispatchState {
        DispatchState::from_raw(self.state.load(Ordering::Acquire))
    }

    pub(super) fn allows_dispatch(&self) -> bool {
        matches!(
            self.state(),
            DispatchState::Running | DispatchState::Draining
        )
    }

    pub(super) fn begin_drain(&self) -> Result<(), DispatchGateError> {
        match self.state() {
            DispatchState::Running => {
                self.transition(DispatchState::Running, DispatchState::Draining)
            }
            DispatchState::Draining => Ok(()),
            DispatchState::Quiesced => Err(DispatchGateError::InvalidState {
                expected: DispatchState::Running,
                actual: DispatchState::Quiesced,
            }),
            DispatchState::Closed => Err(DispatchGateError::Closed),
        }
    }

    pub(super) fn commit_quiesced(
        &self,
        _proof: DispatchCutoffProof,
    ) -> Result<(), DispatchGateError> {
        self.transition(DispatchState::Draining, DispatchState::Quiesced)
    }

    pub(super) fn resume(&self) -> Result<(), DispatchGateError> {
        match self.state() {
            DispatchState::Running => Ok(()),
            DispatchState::Quiesced => {
                self.transition(DispatchState::Quiesced, DispatchState::Running)
            }
            DispatchState::Draining => Err(DispatchGateError::InvalidState {
                expected: DispatchState::Quiesced,
                actual: DispatchState::Draining,
            }),
            DispatchState::Closed => Err(DispatchGateError::Closed),
        }
    }

    pub(super) fn close(&self) -> Result<(), DispatchGateError> {
        match self.state() {
            DispatchState::Closed => Ok(()),
            DispatchState::Quiesced => {
                self.transition(DispatchState::Quiesced, DispatchState::Closed)
            }
            actual => Err(DispatchGateError::InvalidState {
                expected: DispatchState::Quiesced,
                actual,
            }),
        }
    }

    fn transition(
        &self,
        expected: DispatchState,
        next: DispatchState,
    ) -> Result<(), DispatchGateError> {
        self.state
            .compare_exchange(
                expected as u8,
                next as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|actual| {
                let actual = DispatchState::from_raw(actual);
                if actual == DispatchState::Closed {
                    DispatchGateError::Closed
                } else {
                    DispatchGateError::InvalidState { expected, actual }
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freeze_rejects_new_admission_but_waits_for_preexisting_submitters() {
        let gate = AdmissionGate::new();
        let admitted = gate.try_admit().unwrap();

        let progress = gate.begin_freeze().unwrap();

        assert_eq!(progress.active_submitters(), 1);
        assert!(matches!(gate.try_admit(), Err(AdmissionError::Frozen)));
        assert!(!gate.is_frozen_and_idle());
        drop(admitted);
        assert!(gate.is_frozen_and_idle());
    }

    #[test]
    fn dispatch_drain_continues_old_work_until_a_drain_proof_commits() {
        let gate = DispatchGate::new();

        gate.begin_drain().unwrap();

        assert!(gate.allows_dispatch());
        assert_eq!(gate.state(), DispatchState::Draining);
        gate.commit_quiesced(DispatchCutoffProof::for_test())
            .unwrap();
        assert!(!gate.allows_dispatch());
        assert_eq!(gate.state(), DispatchState::Quiesced);
    }

    #[test]
    fn closed_gates_cannot_be_reopened() {
        let admission = AdmissionGate::new();
        admission.begin_freeze().unwrap();
        admission.close().unwrap();
        assert!(matches!(admission.thaw(), Err(AdmissionError::Closed)));

        let dispatch = DispatchGate::new();
        dispatch.begin_drain().unwrap();
        dispatch
            .commit_quiesced(DispatchCutoffProof::for_test())
            .unwrap();
        dispatch.close().unwrap();
        assert!(matches!(dispatch.resume(), Err(DispatchGateError::Closed)));
    }
}
