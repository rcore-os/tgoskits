//! IRQ endpoint adaptation from host snapshots to linear block evidence.

use alloc::{boxed::Box, sync::Arc};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use rdif_block::{
    BlkError, BlockEvidenceSource, ContainmentCause, FaultContainment, IrqCapture, IrqControlError,
    IrqEndpoint, IrqSourceControl, MaskedSource,
};

use super::{SdmmcEvidenceLedger, SdmmcIrqFacts};
use crate::{
    Error,
    rdif::config::map_dev_err_to_blk_err,
    sdio::{HostEvent, HostEventKind, SdioIrqControlError, SdioIrqSource},
};

/// Converts one host IRQ lease into a v0.13 linear-evidence source.
///
/// The returned hard endpoint owns destructive status capture and publishes
/// only opaque ledger identities. Its control endpoint translates every
/// device mask into an independent one-shot mask epoch before runtime sees it.
pub fn into_evidence_source<E, C>(
    source: SdioIrqSource<E, C>,
    ledger: Arc<SdmmcEvidenceLedger>,
    lifecycle_generation: NonZeroU64,
) -> BlockEvidenceSource
where
    E: IrqEndpoint<Fault = Error>,
    E::Event: HostEvent,
    C: IrqSourceControl<Error = SdioIrqControlError>,
{
    into_evidence_source_with_epoch(
        source,
        ledger,
        Arc::new(SdmmcEvidenceEpoch::new(lifecycle_generation)),
    )
}

/// Converts a host IRQ lease while retaining a lifecycle generation that the
/// controller owner advances only after IRQ synchronization and DMA quiesce.
pub fn into_evidence_source_with_epoch<E, C>(
    source: SdioIrqSource<E, C>,
    ledger: Arc<SdmmcEvidenceLedger>,
    lifecycle: Arc<SdmmcEvidenceEpoch>,
) -> BlockEvidenceSource
where
    E: IrqEndpoint<Fault = Error>,
    E::Event: HostEvent,
    C: IrqSourceControl<Error = SdioIrqControlError>,
{
    let (endpoint, control) = source.into_parts();
    let masks = Arc::new(MaskEpochBridge::new(Arc::clone(&lifecycle)));
    BlockEvidenceSource::new(
        Box::new(SdmmcEvidenceEndpoint {
            endpoint,
            ledger,
            masks: Arc::clone(&masks),
            lifecycle,
        }),
        Box::new(SdmmcEvidenceControl { control, masks }),
    )
}

/// Shared generation clock for stale-evidence rejection across reinitialize.
pub struct SdmmcEvidenceEpoch {
    generation: AtomicU64,
}

impl SdmmcEvidenceEpoch {
    pub const fn new(generation: NonZeroU64) -> Self {
        Self {
            generation: AtomicU64::new(generation.get()),
        }
    }

    pub fn current(&self) -> NonZeroU64 {
        NonZeroU64::new(self.generation.load(Ordering::Acquire))
            .unwrap_or_else(|| unreachable!("SD/MMC evidence generation is always nonzero"))
    }

    /// Advances the controller lifecycle after old IRQ and DMA ownership has
    /// been retired.
    pub fn advance(&self) -> Result<NonZeroU64, BlkError> {
        let mut current = self.generation.load(Ordering::Acquire);
        loop {
            let next = current.checked_add(1).ok_or(BlkError::Quarantined)?;
            match self.generation.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return NonZeroU64::new(next).ok_or(BlkError::Quarantined);
                }
                Err(observed) => current = observed,
            }
        }
    }
}

struct SdmmcEvidenceEndpoint<E> {
    endpoint: E,
    ledger: Arc<SdmmcEvidenceLedger>,
    masks: Arc<MaskEpochBridge>,
    lifecycle: Arc<SdmmcEvidenceEpoch>,
}

impl<E> IrqEndpoint for SdmmcEvidenceEndpoint<E>
where
    E: IrqEndpoint<Fault = Error>,
    E::Event: HostEvent,
{
    type Event = rdif_block::IrqEvidenceId;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        match self.endpoint.capture() {
            IrqCapture::Unhandled => IrqCapture::Unhandled,
            IrqCapture::Captured { event, masked } => {
                let facts = facts_from_host_event(&event);
                let masked = match masked
                    .map(|source| self.masks.translate(source))
                    .transpose()
                {
                    Ok(masked) => masked,
                    Err(reason) => return self.contain_after_fault(reason),
                };
                match self.ledger.publish(self.lifecycle.current(), facts) {
                    Ok(event) => IrqCapture::Captured { event, masked },
                    Err(_) => self.contain_after_fault(BlkError::Quarantined),
                }
            }
            IrqCapture::Fault {
                reason,
                containment,
            } => {
                let containment = match containment {
                    FaultContainment::DeviceSourceMasked(source) => {
                        match self.masks.translate(source) {
                            Ok(source) => FaultContainment::DeviceSourceMasked(source),
                            Err(_) => FaultContainment::Uncontained,
                        }
                    }
                    FaultContainment::Uncontained => FaultContainment::Uncontained,
                };
                IrqCapture::Fault {
                    reason: map_dev_err_to_blk_err(reason),
                    containment,
                }
            }
        }
    }

    fn contain(&mut self, cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        let source = self
            .endpoint
            .contain(cause)
            .map_err(map_dev_err_to_blk_err)?;
        self.masks.translate(source)
    }
}

impl<E> SdmmcEvidenceEndpoint<E>
where
    E: IrqEndpoint<Fault = Error>,
    E::Event: HostEvent,
{
    fn contain_after_fault(
        &mut self,
        reason: BlkError,
    ) -> IrqCapture<rdif_block::IrqEvidenceId, BlkError> {
        let containment = self
            .endpoint
            .contain(ContainmentCause::CaptureFault)
            .ok()
            .and_then(|source| self.masks.translate(source).ok())
            .map(FaultContainment::DeviceSourceMasked)
            .unwrap_or(FaultContainment::Uncontained);
        IrqCapture::Fault {
            reason,
            containment,
        }
    }
}

struct SdmmcEvidenceControl<C> {
    control: C,
    masks: Arc<MaskEpochBridge>,
}

impl<C> IrqSourceControl for SdmmcEvidenceControl<C>
where
    C: IrqSourceControl<Error = SdioIrqControlError>,
{
    type Error = IrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        let inner = self.masks.inner_for_rearm(source)?;
        self.control.rearm(inner).map_err(map_irq_control_error)?;
        self.masks.finish_rearm(source)
    }

    fn state(&self) -> Option<rdif_block::IrqSourceState> {
        self.control.state()
    }
}

struct MaskEpochBridge {
    lifecycle: Arc<SdmmcEvidenceEpoch>,
    next_epoch: AtomicU64,
    active_epoch: AtomicU64,
    inner_lifecycle: AtomicU64,
    inner_epoch: AtomicU64,
    inner_bitmap: AtomicU64,
    outer_bitmap: AtomicU64,
}

impl MaskEpochBridge {
    fn new(lifecycle: Arc<SdmmcEvidenceEpoch>) -> Self {
        Self {
            lifecycle,
            next_epoch: AtomicU64::new(0),
            active_epoch: AtomicU64::new(0),
            inner_lifecycle: AtomicU64::new(0),
            inner_epoch: AtomicU64::new(0),
            inner_bitmap: AtomicU64::new(0),
            outer_bitmap: AtomicU64::new(0),
        }
    }

    fn translate(&self, inner: MaskedSource) -> Result<MaskedSource, BlkError> {
        let active = self.active_epoch.load(Ordering::Acquire);
        let epoch = if active == 0 {
            let epoch = self.next_nonzero_epoch()?;
            self.inner_lifecycle
                .store(inner.lifecycle_generation().get(), Ordering::Relaxed);
            self.inner_epoch
                .store(inner.mask_epoch().get(), Ordering::Relaxed);
            self.inner_bitmap
                .store(inner.bitmap().get(), Ordering::Relaxed);
            self.outer_bitmap
                .store(inner.bitmap().get(), Ordering::Relaxed);
            self.active_epoch.store(epoch.get(), Ordering::Release);
            epoch
        } else {
            if self.inner_lifecycle.load(Ordering::Acquire) != inner.lifecycle_generation().get()
                || self.inner_epoch.load(Ordering::Acquire) != inner.mask_epoch().get()
            {
                return Err(BlkError::Quarantined);
            }
            self.inner_bitmap
                .fetch_or(inner.bitmap().get(), Ordering::AcqRel);
            self.outer_bitmap
                .fetch_or(inner.bitmap().get(), Ordering::AcqRel);
            NonZeroU64::new(active).ok_or(BlkError::Quarantined)?
        };
        Ok(MaskedSource::new_with_epoch(
            self.lifecycle.current(),
            epoch,
            inner.bitmap(),
        ))
    }

    fn inner_for_rearm(&self, outer: MaskedSource) -> Result<MaskedSource, IrqControlError> {
        let active = self.active_epoch.load(Ordering::Acquire);
        let lifecycle_generation = self.lifecycle.current();
        if outer.lifecycle_generation() != lifecycle_generation {
            return Err(IrqControlError::StaleGeneration {
                expected: lifecycle_generation.get(),
                actual: outer.lifecycle_generation().get(),
            });
        }
        if outer.mask_epoch().get() != active {
            return Err(IrqControlError::StaleMaskEpoch {
                expected: active,
                actual: outer.mask_epoch().get(),
            });
        }
        let expected_bitmap = self.outer_bitmap.load(Ordering::Acquire);
        if outer.bitmap().get() != expected_bitmap {
            return Err(IrqControlError::SourceNotMasked {
                bitmap: outer.bitmap().get(),
            });
        }
        let lifecycle = NonZeroU64::new(self.inner_lifecycle.load(Ordering::Acquire))
            .ok_or(IrqControlError::Offline)?;
        let epoch = NonZeroU64::new(self.inner_epoch.load(Ordering::Acquire))
            .ok_or(IrqControlError::Offline)?;
        let bitmap = NonZeroU64::new(self.inner_bitmap.load(Ordering::Acquire))
            .ok_or(IrqControlError::Offline)?;
        Ok(MaskedSource::new_with_epoch(lifecycle, epoch, bitmap))
    }

    fn finish_rearm(&self, outer: MaskedSource) -> Result<(), IrqControlError> {
        self.active_epoch
            .compare_exchange(
                outer.mask_epoch().get(),
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|actual| IrqControlError::StaleMaskEpoch {
                expected: outer.mask_epoch().get(),
                actual,
            })?;
        self.inner_bitmap.store(0, Ordering::Release);
        self.outer_bitmap.store(0, Ordering::Release);
        Ok(())
    }

    fn next_nonzero_epoch(&self) -> Result<NonZeroU64, BlkError> {
        let next = self
            .next_epoch
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(BlkError::Quarantined)?;
        Ok(next)
    }
}

fn facts_from_host_event(event: &impl HostEvent) -> SdmmcIrqFacts {
    let summary = event.stable_summary();
    let queue = match event.kind() {
        HostEventKind::Error => SdmmcIrqFacts::error_snapshot(summary),
        HostEventKind::CommandComplete => SdmmcIrqFacts::command_snapshot(summary),
        HostEventKind::TransferComplete
        | HostEventKind::ReceiveReady
        | HostEventKind::TransmitReady => SdmmcIrqFacts::transfer_snapshot(summary),
        HostEventKind::None if !summary.queue_service => SdmmcIrqFacts::side_band_snapshot(summary),
        _ if summary.queue_service => SdmmcIrqFacts::transfer_snapshot(summary),
        _ => SdmmcIrqFacts::side_band_snapshot(summary),
    };
    if summary.card_function_interrupt {
        queue.merge(SdmmcIrqFacts::side_band_snapshot(summary))
    } else {
        queue
    }
}

fn map_irq_control_error(error: SdioIrqControlError) -> IrqControlError {
    match error {
        SdioIrqControlError::StaleGeneration { expected, actual } => {
            IrqControlError::StaleGeneration { expected, actual }
        }
        SdioIrqControlError::SourceNotMasked { bitmap } => {
            IrqControlError::SourceNotMasked { bitmap }
        }
        SdioIrqControlError::Offline => IrqControlError::Offline,
        SdioIrqControlError::Hardware(error) => {
            IrqControlError::Hardware(map_dev_err_to_blk_err(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use super::*;
    use crate::sdio::{HostEventSource, HostEventSummary};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestEvent;

    impl HostEvent for TestEvent {
        fn kind(&self) -> HostEventKind {
            HostEventKind::CommandComplete
        }

        fn source(&self) -> HostEventSource {
            HostEventSource::Command
        }

        fn stable_summary(&self) -> HostEventSummary {
            HostEventSummary {
                stable_status: 1,
                dma_status: 0,
                queue_service: true,
                card_function_interrupt: false,
            }
        }
    }

    struct TestEndpoint;

    impl IrqEndpoint for TestEndpoint {
        type Event = TestEvent;
        type Fault = Error;

        fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
            IrqCapture::Captured {
                event: TestEvent,
                masked: Some(MaskedSource::new(
                    NonZeroU64::new(5).unwrap(),
                    NonZeroU64::MIN,
                )),
            }
        }

        fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
            Ok(MaskedSource::new(
                NonZeroU64::new(5).unwrap(),
                NonZeroU64::MIN,
            ))
        }
    }

    struct TestControl;

    impl IrqSourceControl for TestControl {
        type Error = SdioIrqControlError;

        fn rearm(&mut self, _source: MaskedSource) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn each_armed_to_masked_transition_gets_a_new_outer_epoch() {
        let source = rdif_block::IrqSourceId::new(0).unwrap();
        let ledger = Arc::new(SdmmcEvidenceLedger::new(source, 0));
        let split = into_evidence_source(
            SdioIrqSource::new(TestEndpoint, TestControl),
            ledger,
            NonZeroU64::new(9).unwrap(),
        );
        let (mut endpoint, mut control) = split.into_parts();

        let IrqCapture::Captured {
            masked: Some(first),
            ..
        } = endpoint.capture()
        else {
            panic!("first capture must mask the test source")
        };
        control.rearm(first).unwrap();
        let IrqCapture::Captured {
            masked: Some(second),
            ..
        } = endpoint.capture()
        else {
            panic!("second capture must mask the test source")
        };

        assert_ne!(first.mask_epoch(), second.mask_epoch());
        assert!(matches!(
            control.rearm(first),
            Err(IrqControlError::StaleMaskEpoch { .. })
        ));
        control.rearm(second).unwrap();
    }
}
