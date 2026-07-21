//! Owner-local mask and service ledger for one logical IRQ source.

use rdif_block::{IrqControlError, MaskedSource};
use thiserror::Error;

#[derive(Default)]
pub(super) struct IrqSourceLedger {
    service_pending: bool,
    masked: Option<MaskedSource>,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(in crate::block) enum RuntimeIrqSourceError {
    #[error("block IRQ event named unregistered source {source_id}")]
    UnknownSource { source_id: usize },
    #[error(
        "block IRQ source {source_id} retained generation {retained_generation} but observed \
         generation {observed_generation}"
    )]
    ConflictingGeneration {
        source_id: usize,
        retained_generation: u64,
        observed_generation: u64,
    },
    #[error(
        "block IRQ source {source_id} retained mask epoch {retained_mask_epoch} but observed mask \
         epoch {observed_mask_epoch}"
    )]
    ConflictingMaskEpoch {
        source_id: usize,
        retained_mask_epoch: u64,
        observed_mask_epoch: u64,
    },
    #[error("block IRQ source {source_id} still has unserviced captured facts")]
    ServicePending { source_id: usize },
    #[error(
        "block IRQ source {source_id} rearm failed for lifecycle generation {generation}, mask \
         epoch {mask_epoch}, bitmap {bitmap:#x}: {error}"
    )]
    Rearm {
        source_id: usize,
        generation: u64,
        mask_epoch: u64,
        bitmap: u64,
        error: IrqControlError,
    },
}

impl IrqSourceLedger {
    pub(super) const fn service_pending(&self) -> bool {
        self.service_pending
    }

    #[cfg(test)]
    pub(super) const fn retained_mask(&self) -> Option<MaskedSource> {
        self.masked
    }

    pub(super) fn record_service_fact(
        &mut self,
        source_id: usize,
        masked: Option<MaskedSource>,
    ) -> Result<(), RuntimeIrqSourceError> {
        self.service_pending = true;
        let Some(observed) = masked else {
            return Ok(());
        };
        let Some(retained) = self.masked else {
            self.masked = Some(observed);
            return Ok(());
        };
        if retained.lifecycle_generation() != observed.lifecycle_generation() {
            return Err(RuntimeIrqSourceError::ConflictingGeneration {
                source_id,
                retained_generation: retained.lifecycle_generation().get(),
                observed_generation: observed.lifecycle_generation().get(),
            });
        }
        if retained.mask_epoch() != observed.mask_epoch() {
            return Err(RuntimeIrqSourceError::ConflictingMaskEpoch {
                source_id,
                retained_mask_epoch: retained.mask_epoch().get(),
                observed_mask_epoch: observed.mask_epoch().get(),
            });
        }

        let bitmap = retained.bitmap().get() | observed.bitmap().get();
        self.masked = Some(
            MaskedSource::try_new_with_epoch(
                retained.lifecycle_generation().get(),
                retained.mask_epoch().get(),
                bitmap,
            )
            .expect("the union of nonzero masks from the same source epoch is valid"),
        );
        Ok(())
    }

    pub(super) fn finish_service(&mut self) {
        self.service_pending = false;
    }

    pub(super) fn try_rearm<E>(
        &mut self,
        rearm: impl FnOnce(MaskedSource) -> Result<(), E>,
    ) -> Result<bool, E> {
        if self.service_pending {
            return Ok(false);
        }
        let Some(masked) = self.masked else {
            return Ok(false);
        };
        rearm(masked)?;
        self.masked = None;
        Ok(true)
    }

    pub(super) fn discard_after_device_quiesce(&mut self) {
        self.service_pending = false;
        self.masked = None;
    }
}
