//! Linear per-controller commit/return transactions and partial-failure accounting.

use alloc::vec::Vec;

use super::{BlockControllerIdentity, BlockHandoffError};
use crate::block::controller::{
    ControllerCommitFailure, ControllerHandoffReservation, ControllerReturnFailure,
    GuestOwnedControllerLease, QuarantinedControllerLease,
};

pub(super) trait PreparedControllerTransaction: Sized {
    type GuestOwned: GuestControllerTransaction<Quarantined = Self::Quarantined, Error = Self::Error>;
    type Quarantined;
    type Error;

    fn identity(&self) -> BlockControllerIdentity;

    fn commit(self) -> Result<Self::GuestOwned, (Self::Error, Self::Quarantined)>;
}

pub(super) trait GuestControllerTransaction: Sized {
    type Quarantined;
    type Error;

    fn quarantine(self) -> Self::Quarantined;

    fn return_to_host(self) -> Result<BlockControllerIdentity, (Self::Error, Self::Quarantined)>;
}

pub(super) struct BatchCommitFailure<E, Q> {
    pub(super) source: E,
    pub(super) quarantined: Vec<Q>,
    pub(super) canceled: Vec<BlockControllerIdentity>,
}

pub(super) struct BatchReturnFailure<E, Q> {
    pub(super) source: E,
    pub(super) returned: Vec<BlockControllerIdentity>,
    pub(super) quarantined: Vec<Q>,
}

pub(super) type CommitBatchResult<P> = Result<
    Vec<<P as PreparedControllerTransaction>::GuestOwned>,
    BatchCommitFailure<
        <P as PreparedControllerTransaction>::Error,
        <P as PreparedControllerTransaction>::Quarantined,
    >,
>;

pub(super) type ReturnBatchResult<G> = Result<
    Vec<BlockControllerIdentity>,
    BatchReturnFailure<
        <G as GuestControllerTransaction>::Error,
        <G as GuestControllerTransaction>::Quarantined,
    >,
>;

pub(super) fn commit_controller_batch<P>(controllers: Vec<P>) -> CommitBatchResult<P>
where
    P: PreparedControllerTransaction,
{
    let mut pending = controllers.into_iter();
    let mut guest_owned = Vec::new();
    while let Some(controller) = pending.next() {
        match controller.commit() {
            Ok(controller) => guest_owned.push(controller),
            Err((source, quarantined_controller)) => {
                let canceled = pending
                    .map(|controller| controller.identity())
                    .collect::<Vec<_>>();
                let mut quarantined = guest_owned
                    .into_iter()
                    .map(GuestControllerTransaction::quarantine)
                    .collect::<Vec<_>>();
                quarantined.push(quarantined_controller);
                return Err(BatchCommitFailure {
                    source,
                    quarantined,
                    canceled,
                });
            }
        }
    }
    Ok(guest_owned)
}

pub(super) fn return_controller_batch<G>(controllers: Vec<G>) -> ReturnBatchResult<G>
where
    G: GuestControllerTransaction,
{
    let mut returned = Vec::new();
    let mut quarantined = Vec::new();
    let mut first_error = None;
    for controller in controllers {
        match controller.return_to_host() {
            Ok(identity) => returned.push(identity),
            Err((error, controller)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                quarantined.push(controller);
            }
        }
    }
    if let Some(source) = first_error {
        return Err(BatchReturnFailure {
            source,
            returned,
            quarantined,
        });
    }
    Ok(returned)
}

impl PreparedControllerTransaction for ControllerHandoffReservation {
    type Error = BlockHandoffError;
    type GuestOwned = GuestOwnedControllerLease;
    type Quarantined = QuarantinedControllerLease;

    fn identity(&self) -> BlockControllerIdentity {
        self.identity()
    }

    fn commit(self) -> Result<Self::GuestOwned, (Self::Error, Self::Quarantined)> {
        ControllerHandoffReservation::commit(self)
            .map_err(|ControllerCommitFailure { error, quarantined }| (error, quarantined))
    }
}

impl GuestControllerTransaction for GuestOwnedControllerLease {
    type Error = BlockHandoffError;
    type Quarantined = QuarantinedControllerLease;

    fn quarantine(self) -> Self::Quarantined {
        self.quarantine()
    }

    fn return_to_host(self) -> Result<BlockControllerIdentity, (Self::Error, Self::Quarantined)> {
        self.return_from_guest()
            .map_err(|ControllerReturnFailure { error, quarantined }| (error, quarantined))
    }
}
