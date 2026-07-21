//! Direct lifecycle transitions for the combined SD/MMC owner.

use core::mem;

use rdif_block::{
    ControllerControl, ControllerReady, DmaQuiesced, InitError, InitInput, InitPoll,
    InterruptLifecycle, RecoveryCause,
};

use super::domain::{CombinedSdmmcDomain, RecoveryState};
use crate::{rdif::BlockHost, sdio::OwnedSdioInitHost};

impl<H> InterruptLifecycle for CombinedSdmmcDomain<H>
where
    H: BlockHost + OwnedSdioInitHost,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
    fn controller_cookie(&self) -> usize {
        self.controller_identity().get()
    }

    fn begin_dma_quiesce(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
        cause: RecoveryCause,
    ) -> Result<(), InitError> {
        if self.is_irq_enabled() {
            return Err(InitError::InvalidState);
        }
        let ready = self
            .lifecycle_domain_mut()
            .map_err(|_| InitError::InvalidState)?;
        if !matches!(
            ready.recovery,
            RecoveryState::Idle | RecoveryState::GuestOwned
        ) {
            return Err(InitError::InvalidState);
        }
        let host = H::begin_recovery(ready.card.host_mut(), cause)
            .map_err(|_| lifecycle_error("SD/MMC controller could not enter recovery"))?;
        ready.recovery = RecoveryState::Quiescing { epoch, host };
        Ok(())
    }

    fn poll_dma_quiesce(&mut self, input: InitInput) -> InitPoll<DmaQuiesced> {
        let controller_cookie = self.controller_cookie();
        let ready = match self.lifecycle_domain_mut() {
            Ok(ready) => ready,
            Err(_) => return InitPoll::Failed(InitError::InvalidState),
        };
        let RecoveryState::Quiescing { host, .. } = &mut ready.recovery else {
            return InitPoll::Failed(InitError::InvalidState);
        };
        match H::poll_dma_quiesce(ready.card.host_mut(), host, input) {
            InitPoll::Ready(()) => {
                let recovery = mem::replace(&mut ready.recovery, RecoveryState::Idle);
                let RecoveryState::Quiescing { epoch, host } = recovery else {
                    unreachable!("the SD/MMC recovery state was matched before replacement")
                };
                ready.recovery = RecoveryState::Quiesced { epoch, host };
                // SAFETY: the host-specific FSM returned Ready only after the
                // runtime closed dispatch and IRQ delivery and the controller
                // proved every DMA/PIO engine idle for this exact epoch.
                InitPoll::Ready(unsafe { DmaQuiesced::new(epoch, controller_cookie) })
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => InitPoll::Failed(error),
        }
    }

    fn enter_guest_owned(&mut self, proof: DmaQuiesced) -> Result<(), InitError> {
        if proof.controller_cookie() != self.controller_cookie() {
            return Err(InitError::InvalidState);
        }
        let ready = self
            .lifecycle_domain_mut()
            .map_err(|_| InitError::InvalidState)?;
        let recovery = mem::replace(&mut ready.recovery, RecoveryState::Idle);
        let (epoch, host) = match recovery {
            RecoveryState::Quiesced { epoch, host } => (epoch, host),
            recovery => {
                ready.recovery = recovery;
                return Err(InitError::InvalidState);
            }
        };
        if proof.epoch() != epoch {
            ready.recovery = RecoveryState::Quiesced { epoch, host };
            return Err(InitError::InvalidState);
        }
        drop(host);
        ready.recovery = RecoveryState::GuestOwned;
        Ok(())
    }

    fn begin_reinitialize(&mut self, proof: DmaQuiesced) -> Result<(), InitError> {
        if proof.controller_cookie() != self.controller_cookie() {
            return Err(InitError::InvalidState);
        }
        let ready = self
            .lifecycle_domain_mut()
            .map_err(|_| InitError::InvalidState)?;
        let recovery = mem::replace(&mut ready.recovery, RecoveryState::Idle);
        let RecoveryState::Quiesced { epoch, mut host } = recovery else {
            ready.recovery = recovery;
            return Err(InitError::InvalidState);
        };
        if proof.epoch() != epoch {
            ready.recovery = RecoveryState::Quiesced { epoch, host };
            return Err(InitError::InvalidState);
        }
        if H::begin_reinitialize(ready.card.host_mut(), &mut host).is_err() {
            ready.recovery = RecoveryState::Quiesced { epoch, host };
            return Err(lifecycle_error(
                "SD/MMC controller could not begin reinitialization",
            ));
        }
        ready.recovery = RecoveryState::Reinitializing { epoch, host };
        Ok(())
    }

    fn poll_reinitialize(&mut self, input: InitInput) -> InitPoll<ControllerReady> {
        let controller_cookie = self.controller_cookie();
        let ready = match self.lifecycle_domain_mut() {
            Ok(ready) => ready,
            Err(_) => return InitPoll::Failed(InitError::InvalidState),
        };
        let RecoveryState::Reinitializing { epoch, host } = &mut ready.recovery else {
            return InitPoll::Failed(InitError::InvalidState);
        };
        match H::poll_reinitialize(ready.card.host_mut(), host, input) {
            InitPoll::Ready(()) => {
                let epoch = *epoch;
                ready.recovery = RecoveryState::Idle;
                // SAFETY: the host-specific rebuild returned Ready only after
                // restoring clocks, bus state, queue storage, DMA state and
                // device-side IRQ configuration for this epoch.
                InitPoll::Ready(unsafe { ControllerReady::new(epoch, controller_cookie) })
            }
            InitPoll::Pending(schedule) => InitPoll::Pending(schedule),
            InitPoll::Failed(error) => InitPoll::Failed(error),
        }
    }
}

const fn lifecycle_error(message: &'static str) -> InitError {
    InitError::Hardware(message)
}
