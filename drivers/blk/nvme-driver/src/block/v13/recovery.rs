//! NVMe v0.13 controller lifecycle routing after final publication.

use alloc::vec::Vec;

use rdif_block::{
    ControllerEpoch, ControllerReady, DmaQuiesced, InitError, InitInput, InitPoll,
    OwnershipDomainId,
};

use crate::lifecycle::{LifecycleHardware, NvmeLifecycle};

/// Recovery phase visible to the v0.13 control-trigger router.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NvmeV13RecoveryPhase {
    Running,
    Quiescing,
    Quiesced,
    Reinitializing,
    GuestOwned,
    Failed,
}

/// One controller lifecycle plus the immutable set of domains it rebuilds.
pub(super) struct NvmeV13Recovery {
    lifecycle: NvmeLifecycle,
    phase: NvmeV13RecoveryPhase,
    domains: Vec<OwnershipDomainId>,
}

impl NvmeV13Recovery {
    pub(super) fn new(domains: Vec<OwnershipDomainId>) -> Self {
        Self {
            lifecycle: NvmeLifecycle::new(),
            phase: NvmeV13RecoveryPhase::Running,
            domains,
        }
    }

    pub(super) const fn phase(&self) -> NvmeV13RecoveryPhase {
        self.phase
    }

    pub(super) fn domains(&self) -> Vec<OwnershipDomainId> {
        self.domains.clone()
    }

    pub(super) fn begin_quiesce(
        &mut self,
        hardware: &mut impl LifecycleHardware,
        epoch: ControllerEpoch,
    ) -> Result<(), InitError> {
        if !matches!(
            self.phase,
            NvmeV13RecoveryPhase::Running | NvmeV13RecoveryPhase::GuestOwned
        ) {
            return Err(InitError::InvalidState);
        }
        self.lifecycle.begin_quiesce(hardware, epoch)?;
        self.phase = NvmeV13RecoveryPhase::Quiescing;
        Ok(())
    }

    pub(super) fn poll_quiesce(
        &mut self,
        hardware: &mut impl LifecycleHardware,
        input: InitInput,
    ) -> InitPoll<DmaQuiesced> {
        if self.phase != NvmeV13RecoveryPhase::Quiescing {
            return InitPoll::Failed(InitError::InvalidState);
        }
        match self.lifecycle.poll_dma_quiesce(hardware, input) {
            InitPoll::Ready(proof) => {
                self.phase = NvmeV13RecoveryPhase::Quiesced;
                InitPoll::Ready(proof)
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => {
                self.phase = NvmeV13RecoveryPhase::Failed;
                InitPoll::Failed(error)
            }
        }
    }

    pub(super) fn enter_guest_owned(
        &mut self,
        hardware: &mut impl LifecycleHardware,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        if self.phase != NvmeV13RecoveryPhase::Quiesced {
            return Err(InitError::InvalidState);
        }
        self.lifecycle.enter_guest_owned(hardware, quiesced)?;
        self.phase = NvmeV13RecoveryPhase::GuestOwned;
        Ok(())
    }

    pub(super) fn begin_reinitialize(
        &mut self,
        hardware: &mut impl LifecycleHardware,
        quiesced: DmaQuiesced,
    ) -> Result<(), InitError> {
        if self.phase != NvmeV13RecoveryPhase::Quiesced {
            return Err(InitError::InvalidState);
        }
        self.lifecycle.begin_reinitialize(hardware, quiesced)?;
        self.phase = NvmeV13RecoveryPhase::Reinitializing;
        Ok(())
    }

    pub(super) fn poll_reinitialize(
        &mut self,
        hardware: &mut impl LifecycleHardware,
        input: InitInput,
    ) -> InitPoll<ControllerReady> {
        if self.phase != NvmeV13RecoveryPhase::Reinitializing {
            return InitPoll::Failed(InitError::InvalidState);
        }
        match self.lifecycle.poll_reinitialize(hardware, input) {
            InitPoll::Ready(ready) => {
                self.phase = NvmeV13RecoveryPhase::Running;
                InitPoll::Ready(ready)
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => {
                self.phase = NvmeV13RecoveryPhase::Failed;
                InitPoll::Failed(error)
            }
        }
    }
}
