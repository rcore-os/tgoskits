//! Fail-closed ownership for queue memory reachable by controller DMA.

use core::{fmt, mem, mem::ManuallyDrop, ptr};

use rdif_block::{BlkError, ControllerEpoch, DmaQuiesced};

/// Linear owner for one live SQ/CQ and all request backing retained with it.
///
/// Ordinary drop converts a live owner into a named quarantine whose
/// `ManuallyDrop` storage remains allocated. Only a matching, strictly newer
/// controller-quiescence proof can authorize ordinary Rust destruction.
pub(super) enum QueueDmaOwner<T> {
    Live(LiveQueueDmaOwner<T>),
    Closed,
    Quarantined(QuarantinedQueueDmaOwner<T>),
}

pub(super) struct LiveQueueDmaOwner<T> {
    storage: ManuallyDrop<T>,
    binding: Option<QueueDmaBinding>,
    quiesced_epoch: Option<ControllerEpoch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueueDmaBinding {
    controller_cookie: usize,
    active_epoch: ControllerEpoch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueDmaQuarantineReason {
    DroppedWithoutQuiescence,
}

/// Named retention of queue memory which hardware may still reach.
///
/// There is deliberately no implicit recovery path here. Controller recovery
/// must keep the enclosing domain alive and close it explicitly while holding
/// the matching proof; abandoning that transaction retains the allocation.
pub(super) struct QuarantinedQueueDmaOwner<T> {
    storage: ManuallyDrop<T>,
    binding: Option<QueueDmaBinding>,
    reason: QueueDmaQuarantineReason,
}

impl<T> QueueDmaOwner<T> {
    pub(super) fn new(storage: T) -> Self {
        Self::Live(LiveQueueDmaOwner {
            storage: ManuallyDrop::new(storage),
            binding: None,
            quiesced_epoch: None,
        })
    }

    pub(super) fn bind(
        &mut self,
        controller_cookie: usize,
        active_epoch: ControllerEpoch,
    ) -> Result<(), BlkError> {
        if controller_cookie == 0 || active_epoch.get() == 0 {
            return Err(BlkError::InvalidDmaProof);
        }
        let live = self.live_owner_mut()?;
        if live.binding.is_some() || live.quiesced_epoch.is_some() {
            return Err(BlkError::InvalidDmaProof);
        }
        live.binding = Some(QueueDmaBinding {
            controller_cookie,
            active_epoch,
        });
        Ok(())
    }

    pub(super) fn live(&self) -> Option<&T> {
        match self {
            Self::Live(live) => Some(&live.storage),
            Self::Closed => None,
            Self::Quarantined(quarantine) => {
                let _retained_owner = quarantine;
                None
            }
        }
    }

    pub(super) fn live_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Live(live) => Some(&mut live.storage),
            Self::Closed => None,
            Self::Quarantined(quarantine) => {
                let _retained_owner = quarantine;
                None
            }
        }
    }

    pub(super) fn validate_quiescence(&self, proof: &DmaQuiesced) -> Result<(), BlkError> {
        let live = self.live_owner()?;
        let binding = live.binding.ok_or(BlkError::InvalidDmaProof)?;
        if proof.controller_cookie() != binding.controller_cookie
            || proof.epoch() <= binding.active_epoch
            || live.quiesced_epoch.is_some()
        {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    pub(super) fn record_quiesced(&mut self, proof: &DmaQuiesced) -> Result<(), BlkError> {
        self.validate_quiescence(proof)?;
        self.live_owner_mut()?.quiesced_epoch = Some(proof.epoch());
        Ok(())
    }

    pub(super) fn resume_after_reinitialize(
        &mut self,
        epoch: ControllerEpoch,
    ) -> Result<(), BlkError> {
        self.validate_resume(epoch)?;
        let live = self.live_owner_mut()?;
        let binding = live.binding.as_mut().ok_or(BlkError::InvalidDmaProof)?;
        binding.active_epoch = epoch;
        live.quiesced_epoch = None;
        Ok(())
    }

    pub(super) fn validate_resume(&self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        let live = self.live_owner()?;
        let binding = live.binding.ok_or(BlkError::InvalidDmaProof)?;
        if live.quiesced_epoch != Some(epoch) || epoch <= binding.active_epoch {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    pub(super) fn validate_close(&self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        let live = self.live_owner()?;
        if live.quiesced_epoch != Some(epoch) {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    pub(super) fn close_after_quiesce(&mut self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        let storage = self.take_after_quiesce(epoch)?;
        drop(storage);
        Ok(())
    }

    #[cfg(test)]
    pub(super) const fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }

    fn take_after_quiesce(&mut self, epoch: ControllerEpoch) -> Result<T, BlkError> {
        self.validate_close(epoch)?;

        let mut previous = ManuallyDrop::new(mem::replace(self, Self::Closed));
        let Self::Live(live) = &mut *previous else {
            // The validation above proved this was live. Retain the prior
            // owner if that invariant changes during future refactoring.
            let previous = unsafe {
                // SAFETY: `previous` has not been moved and remains a complete
                // `QueueDmaOwner`; restoring it transfers that ownership back.
                ManuallyDrop::take(&mut previous)
            };
            *self = previous;
            return Err(BlkError::InvalidDmaProof);
        };
        Ok(unsafe {
            // SAFETY: `self` is now Closed, the exact quiesced epoch was
            // validated, and `previous` is suppressed by `ManuallyDrop`.
            ManuallyDrop::take(&mut live.storage)
        })
    }

    fn live_owner(&self) -> Result<&LiveQueueDmaOwner<T>, BlkError> {
        match self {
            Self::Live(live) => Ok(live),
            Self::Closed => Err(BlkError::Offline),
            Self::Quarantined(_) => Err(BlkError::Quarantined),
        }
    }

    fn live_owner_mut(&mut self) -> Result<&mut LiveQueueDmaOwner<T>, BlkError> {
        match self {
            Self::Live(live) => Ok(live),
            Self::Closed => Err(BlkError::Offline),
            Self::Quarantined(_) => Err(BlkError::Quarantined),
        }
    }
}

impl<T> Drop for QueueDmaOwner<T> {
    fn drop(&mut self) {
        let Self::Live(live) = self else {
            return;
        };
        let quarantine = QuarantinedQueueDmaOwner {
            storage: unsafe {
                // SAFETY: the live field is immediately overwritten without
                // running its destructor, transferring its sole storage owner
                // into the quarantine value.
                ManuallyDrop::new(ManuallyDrop::take(&mut live.storage))
            },
            binding: live.binding,
            reason: QueueDmaQuarantineReason::DroppedWithoutQuiescence,
        };
        unsafe {
            // SAFETY: `live.storage` was moved above. Overwriting `self`
            // prevents drop glue from observing that moved field, and the new
            // quarantine deliberately suppresses destruction of the storage.
            ptr::write(self, Self::Quarantined(quarantine));
        }
    }
}

impl<T> fmt::Debug for QuarantinedQueueDmaOwner<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuarantinedQueueDmaOwner")
            .field("binding", &self.binding)
            .field("reason", &self.reason)
            .field("storage", &core::ptr::from_ref(&*self.storage))
            .finish()
    }
}
