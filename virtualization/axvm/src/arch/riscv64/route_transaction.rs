// Typed ownership gate for the RISC-V platform IRQ route transaction.

use core::marker::PhantomData;

#[cfg(not(test))]
pub(super) type RouteControl<T> = ax_kspin::SpinNoPreempt<T>;

#[cfg(test)]
pub(super) type RouteControl<T> =
    ax_kspin::SpinMutex<ax_kspin::RawSpinLock<ax_kspin::RawContext>, T>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoutePhase<Key> {
    Vacant,
    Reserved {
        key: Key,
        generation: u64,
    },
    Published {
        key: Key,
        generation: u64,
    },
    Activating {
        key: Key,
        generation: u64,
    },
    Active {
        key: Key,
        generation: u64,
    },
    Revoking {
        key: Key,
        generation: u64,
        revoker_active: bool,
    },
}

/// Largest generation that can be packed with a two-bit publication phase.
pub(crate) const ROUTE_GENERATION_MAX: u64 = u64::MAX >> 2;

/// Short-lock state for one generation-scoped route.
///
/// The state stores the complete canonical owner key. The lock protects only
/// phase and generation changes; allocation, controller leasing, publication,
/// and MMIO activation happen while a typed permit owns the phase.
pub(crate) struct RouteTransactionState<Key> {
    phase: RoutePhase<Key>,
    next_generation: u64,
}

impl<Key> RouteTransactionState<Key> {
    pub(crate) const fn new() -> Self {
        Self {
            phase: RoutePhase::Vacant,
            next_generation: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreparationReservation {
    Existing,
    Reserved { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationReservation {
    Existing,
    Reserved { generation: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RevocationReservation {
    Vacant,
    Reserved { generation: u64 },
}

impl<Key: Copy + Eq> RouteTransactionState<Key> {
    fn identity(&self) -> Option<(Key, u64)> {
        match self.phase {
            RoutePhase::Vacant => None,
            RoutePhase::Reserved { key, generation }
            | RoutePhase::Published { key, generation }
            | RoutePhase::Activating { key, generation }
            | RoutePhase::Active { key, generation }
            | RoutePhase::Revoking {
                key, generation, ..
            } => Some((key, generation)),
        }
    }

    fn reserve_preparation(
        &mut self,
        key: Key,
    ) -> Result<PreparationReservation, RouteReservationError> {
        match self.phase {
            RoutePhase::Vacant => {
                let generation = next_generation(self.next_generation);
                self.next_generation = generation;
                self.phase = RoutePhase::Reserved { key, generation };
                Ok(PreparationReservation::Reserved { generation })
            }
            RoutePhase::Reserved { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Preparing)
            }
            RoutePhase::Published { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Published)
            }
            RoutePhase::Activating { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Activating)
            }
            RoutePhase::Active { key: installed, .. } if installed == key => {
                Ok(PreparationReservation::Existing)
            }
            RoutePhase::Revoking { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Revoking)
            }
            _ => Err(RouteReservationError::Conflicting),
        }
    }

    fn reserve_activation(
        &mut self,
        key: Key,
    ) -> Result<ActivationReservation, RouteReservationError> {
        match self.phase {
            RoutePhase::Published {
                key: installed,
                generation,
            } if installed == key => {
                self.phase = RoutePhase::Activating { key, generation };
                Ok(ActivationReservation::Reserved { generation })
            }
            RoutePhase::Active { key: installed, .. } if installed == key => {
                Ok(ActivationReservation::Existing)
            }
            RoutePhase::Reserved { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Preparing)
            }
            RoutePhase::Activating { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Activating)
            }
            RoutePhase::Revoking { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Revoking)
            }
            RoutePhase::Vacant => Err(RouteReservationError::Vacant),
            _ => Err(RouteReservationError::Conflicting),
        }
    }

    fn reserve_revocation(
        &mut self,
        key: Key,
    ) -> Result<RevocationReservation, RouteReservationError> {
        match self.phase {
            RoutePhase::Vacant => Ok(RevocationReservation::Vacant),
            RoutePhase::Active {
                key: installed,
                generation,
            } if installed == key => {
                self.phase = RoutePhase::Revoking {
                    key,
                    generation,
                    revoker_active: true,
                };
                Ok(RevocationReservation::Reserved { generation })
            }
            RoutePhase::Revoking {
                key: installed,
                generation,
                revoker_active: false,
            } if installed == key => {
                self.phase = RoutePhase::Revoking {
                    key,
                    generation,
                    revoker_active: true,
                };
                Ok(RevocationReservation::Reserved { generation })
            }
            RoutePhase::Revoking { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Revoking)
            }
            RoutePhase::Reserved { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Preparing)
            }
            RoutePhase::Published { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Published)
            }
            RoutePhase::Activating { key: installed, .. } if installed == key => {
                Err(RouteReservationError::Activating)
            }
            _ => Err(RouteReservationError::Conflicting),
        }
    }

    fn publish_preparation(&mut self, key: Key, generation: u64) {
        assert!(
            self.phase == (RoutePhase::Reserved { key, generation }),
            "RISC-V route preparation permit lost its reserved generation"
        );
        self.phase = RoutePhase::Published { key, generation };
    }

    fn rollback_preparation(&mut self, key: Key, generation: u64) {
        assert!(
            self.phase == (RoutePhase::Reserved { key, generation }),
            "RISC-V route rollback observed a different reserved generation"
        );
        self.phase = RoutePhase::Vacant;
    }

    fn finish_activation(&mut self, key: Key, generation: u64) {
        assert!(
            self.phase == (RoutePhase::Activating { key, generation }),
            "RISC-V route activation permit lost its published generation"
        );
        self.phase = RoutePhase::Active { key, generation };
    }

    fn rollback_activation(&mut self, key: Key, generation: u64) {
        assert!(
            self.phase == (RoutePhase::Activating { key, generation }),
            "RISC-V route activation rollback observed a different generation"
        );
        self.phase = RoutePhase::Published { key, generation };
    }

    fn pause_revocation(&mut self, key: Key, generation: u64) {
        assert!(
            self.phase
                == (RoutePhase::Revoking {
                    key,
                    generation,
                    revoker_active: true,
                }),
            "RISC-V route revocation retry lost its generation"
        );
        self.phase = RoutePhase::Revoking {
            key,
            generation,
            revoker_active: false,
        };
    }

    fn finish_revocation(&mut self, key: Key, generation: u64) {
        assert!(
            self.phase
                == (RoutePhase::Revoking {
                    key,
                    generation,
                    revoker_active: true,
                }),
            "RISC-V route revocation completion lost its generation"
        );
        self.phase = RoutePhase::Vacant;
    }
}

/// Result of reserving the preparation phase.
pub(crate) enum RoutePreparation<Key: Copy + Eq + 'static> {
    /// The exact canonical route is already active.
    Existing,
    /// This caller exclusively owns controller preparation.
    Reserved(RoutePreparePermit<Key>),
}

/// Result of reserving the activation phase.
pub(crate) enum RouteActivation<Key: Copy + Eq + 'static> {
    /// The exact canonical route is already active.
    Existing,
    /// This caller exclusively owns activation.
    Reserved(RouteActivatePermit<Key>),
}

/// Result of reserving fail-closed route revocation.
pub(crate) enum RouteRevocation<Key: Copy + Eq + 'static> {
    /// No route is installed, so revocation is already complete.
    Vacant,
    /// This caller owns one bounded revocation attempt.
    Reserved(RouteRevokePermit<Key>),
}

/// A route could not enter the requested transaction phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteReservationError {
    /// A different canonical owner is installed or in flight.
    Conflicting,
    /// The same owner is currently preparing the controller route.
    Preparing,
    /// The same owner is published but has not begun activation.
    Published,
    /// The same owner is currently activating physical endpoints.
    Activating,
    /// No prepared route exists for activation.
    Vacant,
    /// The matching route is quarantined while revocation is in progress.
    Revoking,
}

/// Reserves an unowned route without holding the control lock across work.
pub(crate) fn prepare_route_if_available<Key: Copy + Eq>(
    control: &'static RouteControl<RouteTransactionState<Key>>,
    key: Key,
) -> Result<RoutePreparation<Key>, RouteReservationError> {
    let mut state = control.lock();
    match state.reserve_preparation(key)? {
        PreparationReservation::Existing => Ok(RoutePreparation::Existing),
        PreparationReservation::Reserved { generation } => {
            Ok(RoutePreparation::Reserved(RoutePreparePermit {
                control,
                key,
                generation,
                rollback: true,
                not_send: PhantomData,
            }))
        }
    }
}

/// Reserves activation of an already published route.
pub(crate) fn activate_published_route<Key: Copy + Eq>(
    control: &'static RouteControl<RouteTransactionState<Key>>,
    key: Key,
) -> Result<RouteActivation<Key>, RouteReservationError> {
    let mut state = control.lock();
    match state.reserve_activation(key)? {
        ActivationReservation::Existing => Ok(RouteActivation::Existing),
        ActivationReservation::Reserved { generation } => {
            Ok(RouteActivation::Reserved(RouteActivatePermit {
                control,
                key,
                generation,
                rollback: true,
                not_send: PhantomData,
            }))
        }
    }
}

/// Reserves one bounded revocation attempt for an active route.
pub(crate) fn revoke_active_route<Key: Copy + Eq>(
    control: &'static RouteControl<RouteTransactionState<Key>>,
    key: Key,
) -> Result<RouteRevocation<Key>, RouteReservationError> {
    let mut state = control.lock();
    match state.reserve_revocation(key)? {
        RevocationReservation::Vacant => Ok(RouteRevocation::Vacant),
        RevocationReservation::Reserved { generation } => {
            Ok(RouteRevocation::Reserved(RouteRevokePermit {
                control,
                key,
                generation,
                finished: false,
                not_send: PhantomData,
            }))
        }
    }
}

/// Returns the canonical identity currently protected by the control lock.
pub(crate) fn current_route_identity<Key: Copy + Eq>(
    control: &'static RouteControl<RouteTransactionState<Key>>,
) -> Option<(Key, u64)> {
    control.lock().identity()
}

/// Exclusive permission to perform route preparation outside the state lock.
pub(crate) struct RoutePreparePermit<Key: Copy + Eq + 'static> {
    control: &'static RouteControl<RouteTransactionState<Key>>,
    key: Key,
    generation: u64,
    rollback: bool,
    not_send: PhantomData<*mut ()>,
}

impl<Key: Copy + Eq> RoutePreparePermit<Key> {
    /// Returns the route generation owned by this permit.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Quarantines the reservation after an irreversible lower-layer commit.
    ///
    /// Once a controller lease or lower-layer publication succeeds, rollback
    /// to vacant would permit a second owner to race permanent hardware state.
    pub(crate) fn begin_irreversible(&mut self) {
        self.rollback = false;
    }

    /// Commits a fully published, still-masked controller route.
    pub(crate) fn publish(mut self) {
        let mut state = self.control.lock();
        state.publish_preparation(self.key, self.generation);
        self.rollback = false;
    }
}

impl<Key: Copy + Eq> Drop for RoutePreparePermit<Key> {
    fn drop(&mut self) {
        if !self.rollback {
            return;
        }
        let mut state = self.control.lock();
        state.rollback_preparation(self.key, self.generation);
    }
}

/// Exclusive permission to activate a published route outside the state lock.
pub(crate) struct RouteActivatePermit<Key: Copy + Eq + 'static> {
    control: &'static RouteControl<RouteTransactionState<Key>>,
    key: Key,
    generation: u64,
    rollback: bool,
    not_send: PhantomData<*mut ()>,
}

impl<Key: Copy + Eq> RouteActivatePermit<Key> {
    /// Quarantines the transaction before an infallible external commit.
    ///
    /// After this call, a panic or unexpected platform error must leave the
    /// route in the activating phase; it cannot be retried as merely
    /// published because physical MMIO may already be visible.
    pub(crate) fn begin_irreversible(&mut self) {
        self.rollback = false;
    }

    /// Commits activation after every endpoint has been made observable.
    pub(crate) fn finish(mut self) {
        let mut state = self.control.lock();
        state.finish_activation(self.key, self.generation);
        self.rollback = false;
    }
}

impl<Key: Copy + Eq> Drop for RouteActivatePermit<Key> {
    fn drop(&mut self) {
        if !self.rollback {
            return;
        }
        let mut state = self.control.lock();
        state.rollback_activation(self.key, self.generation);
    }
}

/// Exclusive permission for one bounded revocation attempt.
pub(crate) struct RouteRevokePermit<Key: Copy + Eq + 'static> {
    control: &'static RouteControl<RouteTransactionState<Key>>,
    key: Key,
    generation: u64,
    finished: bool,
    not_send: PhantomData<*mut ()>,
}

impl<Key: Copy + Eq> RouteRevokePermit<Key> {
    /// Returns the generation that remains quarantined across retries.
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// Publishes completion and permits a later generation to acquire ownership.
    pub(crate) fn finish(mut self) {
        let mut state = self.control.lock();
        state.finish_revocation(self.key, self.generation);
        self.finished = true;
    }
}

impl<Key: Copy + Eq> Drop for RouteRevokePermit<Key> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut state = self.control.lock();
        state.pause_revocation(self.key, self.generation);
    }
}

const fn next_generation(current: u64) -> u64 {
    let next = current.wrapping_add(1) & ROUTE_GENERATION_MAX;
    if next == 0 { 1 } else { next }
}

#[cfg(test)]
mod tests {
    use super::*;

    static CONTROL: RouteControl<RouteTransactionState<u64>> =
        RouteControl::new(RouteTransactionState::new());
    static PREPARATION_QUARANTINE_CONTROL: RouteControl<RouteTransactionState<u64>> =
        RouteControl::new(RouteTransactionState::new());
    static ACTIVATION_QUARANTINE_CONTROL: RouteControl<RouteTransactionState<u64>> =
        RouteControl::new(RouteTransactionState::new());
    static REVOCATION_RETRY_CONTROL: RouteControl<RouteTransactionState<u64>> =
        RouteControl::new(RouteTransactionState::new());
    static IDENTITY_CONTROL: RouteControl<RouteTransactionState<u64>> =
        RouteControl::new(RouteTransactionState::new());

    #[test]
    fn failed_preparation_rolls_back_only_its_reserved_generation() {
        let RoutePreparation::Reserved(first) = prepare_route_if_available(&CONTROL, 11).unwrap()
        else {
            panic!("vacant route must be reserved");
        };
        assert!(matches!(
            prepare_route_if_available(&CONTROL, 11),
            Err(RouteReservationError::Preparing)
        ));
        assert!(matches!(
            prepare_route_if_available(&CONTROL, 12),
            Err(RouteReservationError::Conflicting)
        ));
        drop(first);

        let RoutePreparation::Reserved(second) = prepare_route_if_available(&CONTROL, 12).unwrap()
        else {
            panic!("rolled-back route must be reservable");
        };
        second.publish();
        let RouteActivation::Reserved(active) = activate_published_route(&CONTROL, 12).unwrap()
        else {
            panic!("published route must be activatable");
        };
        active.finish();
        assert!(matches!(
            prepare_route_if_available(&CONTROL, 12),
            Ok(RoutePreparation::Existing)
        ));
        assert!(matches!(
            prepare_route_if_available(&CONTROL, 11),
            Err(RouteReservationError::Conflicting)
        ));
    }

    #[test]
    fn irreversible_preparation_never_reopens_vacant_ownership() {
        let RoutePreparation::Reserved(mut permit) =
            prepare_route_if_available(&PREPARATION_QUARANTINE_CONTROL, 17).unwrap()
        else {
            panic!("vacant route must be reserved");
        };
        permit.begin_irreversible();
        drop(permit);

        assert!(matches!(
            prepare_route_if_available(&PREPARATION_QUARANTINE_CONTROL, 17),
            Err(RouteReservationError::Preparing)
        ));
        assert!(matches!(
            prepare_route_if_available(&PREPARATION_QUARANTINE_CONTROL, 18),
            Err(RouteReservationError::Conflicting)
        ));
    }

    #[test]
    fn irreversible_activation_never_reopens_published_ownership() {
        let RoutePreparation::Reserved(preparation) =
            prepare_route_if_available(&ACTIVATION_QUARANTINE_CONTROL, 23).unwrap()
        else {
            panic!("vacant route must be reserved");
        };
        preparation.publish();
        let RouteActivation::Reserved(mut activation) =
            activate_published_route(&ACTIVATION_QUARANTINE_CONTROL, 23).unwrap()
        else {
            panic!("published route must be activatable");
        };
        activation.begin_irreversible();
        drop(activation);

        assert!(matches!(
            activate_published_route(&ACTIVATION_QUARANTINE_CONTROL, 23),
            Err(RouteReservationError::Activating)
        ));
        assert!(matches!(
            prepare_route_if_available(&ACTIVATION_QUARANTINE_CONTROL, 24),
            Err(RouteReservationError::Conflicting)
        ));
    }

    #[test]
    fn failed_revocation_stays_quarantined_and_retries_the_same_generation() {
        let RoutePreparation::Reserved(preparation) =
            prepare_route_if_available(&REVOCATION_RETRY_CONTROL, 31).unwrap()
        else {
            panic!("vacant route must be reserved");
        };
        preparation.publish();
        let RouteActivation::Reserved(activation) =
            activate_published_route(&REVOCATION_RETRY_CONTROL, 31).unwrap()
        else {
            panic!("published route must be activatable");
        };
        activation.finish();

        let RouteRevocation::Reserved(first) =
            revoke_active_route(&REVOCATION_RETRY_CONTROL, 31).unwrap()
        else {
            panic!("active route must reserve revocation");
        };
        let generation = first.generation();
        assert!(matches!(
            prepare_route_if_available(&REVOCATION_RETRY_CONTROL, 31),
            Err(RouteReservationError::Revoking)
        ));
        assert!(matches!(
            revoke_active_route(&REVOCATION_RETRY_CONTROL, 31),
            Err(RouteReservationError::Revoking)
        ));
        drop(first);

        let RouteRevocation::Reserved(retry) =
            revoke_active_route(&REVOCATION_RETRY_CONTROL, 31).unwrap()
        else {
            panic!("quarantined route must be retryable");
        };
        assert_eq!(retry.generation(), generation);
        retry.finish();

        let RoutePreparation::Reserved(next) =
            prepare_route_if_available(&REVOCATION_RETRY_CONTROL, 32).unwrap()
        else {
            panic!("completed revocation must reopen ownership");
        };
        assert_ne!(next.generation(), generation);
    }

    #[test]
    fn route_identity_remains_visible_without_filesystem_features() {
        assert_eq!(current_route_identity(&IDENTITY_CONTROL), None);
        let RoutePreparation::Reserved(preparation) =
            prepare_route_if_available(&IDENTITY_CONTROL, 41).unwrap()
        else {
            panic!("vacant route must be reserved");
        };
        let generation = preparation.generation();
        assert_eq!(
            current_route_identity(&IDENTITY_CONTROL),
            Some((41, generation))
        );
        preparation.publish();
        let RouteActivation::Reserved(activation) =
            activate_published_route(&IDENTITY_CONTROL, 41).unwrap()
        else {
            panic!("published route must be activatable");
        };
        activation.finish();
        let RouteRevocation::Reserved(revocation) =
            revoke_active_route(&IDENTITY_CONTROL, 41).unwrap()
        else {
            panic!("active route must be revocable");
        };
        assert_eq!(
            current_route_identity(&IDENTITY_CONTROL),
            Some((41, generation))
        );
        revocation.finish();
        assert_eq!(current_route_identity(&IDENTITY_CONTROL), None);
    }
}
