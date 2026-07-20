//! SD/MMC host-event adaptation to the generic RDIF IRQ boundary.

use crate::{
    Error,
    rdif::config::map_dev_err_to_blk_err,
    sdio::{
        HostEvent, HostEventKind, SdioIrqControlError, SdioIrqSource,
        block_queue_ready_from_host_event,
    },
};

/// Converts one controller-owned source into separately owned RDIF endpoints.
pub(super) fn into_block_irq_source<E, C>(source: SdioIrqSource<E, C>) -> rdif_block::BlockIrqSource
where
    E: rdif_irq::IrqEndpoint<Fault = Error>,
    E::Event: HostEvent,
    C: rdif_irq::IrqSourceControl<Error = SdioIrqControlError>,
{
    let (endpoint, control) = source.into_parts();
    rdif_block::BlockIrqSource::new(
        alloc::boxed::Box::new(BlockIrqEndpoint { endpoint }),
        alloc::boxed::Box::new(BlockIrqControl { control }),
    )
}

pub(super) struct BlockIrqEndpoint<E> {
    endpoint: E,
}

impl<E> rdif_irq::IrqEndpoint for BlockIrqEndpoint<E>
where
    E: rdif_irq::IrqEndpoint<Fault = Error>,
    E::Event: HostEvent,
{
    type Event = rdif_block::Event;
    type Fault = rdif_block::BlkError;

    fn capture(&mut self) -> rdif_irq::IrqCapture<Self::Event, Self::Fault> {
        match self.endpoint.capture() {
            rdif_irq::IrqCapture::Unhandled => rdif_irq::IrqCapture::Unhandled,
            rdif_irq::IrqCapture::Captured { event, masked } => rdif_irq::IrqCapture::Captured {
                event: block_event_from_host(&event),
                masked,
            },
            rdif_irq::IrqCapture::Fault {
                reason,
                containment,
            } => rdif_irq::IrqCapture::Fault {
                reason: map_dev_err_to_blk_err(reason),
                containment,
            },
        }
    }

    fn contain(
        &mut self,
        cause: rdif_irq::ContainmentCause,
    ) -> Result<rdif_irq::MaskedSource, Self::Fault> {
        self.endpoint.contain(cause).map_err(map_dev_err_to_blk_err)
    }
}

pub(super) struct BlockIrqControl<C> {
    control: C,
}

impl<C> rdif_irq::IrqSourceControl for BlockIrqControl<C>
where
    C: rdif_irq::IrqSourceControl<Error = SdioIrqControlError>,
{
    type Error = rdif_block::IrqControlError;

    fn rearm(&mut self, source: rdif_irq::MaskedSource) -> Result<(), Self::Error> {
        self.control.rearm(source).map_err(map_irq_control_error)
    }
}

fn map_irq_control_error(error: SdioIrqControlError) -> rdif_block::IrqControlError {
    match error {
        SdioIrqControlError::StaleGeneration { expected, actual } => {
            rdif_block::IrqControlError::StaleGeneration { expected, actual }
        }
        SdioIrqControlError::SourceNotMasked { bitmap } => {
            rdif_block::IrqControlError::SourceNotMasked { bitmap }
        }
        SdioIrqControlError::Offline => rdif_block::IrqControlError::Offline,
        SdioIrqControlError::Hardware(error) => {
            rdif_block::IrqControlError::Hardware(map_dev_err_to_blk_err(error))
        }
    }
}

fn block_event_from_host(host_event: &impl HostEvent) -> rdif_block::Event {
    let mut event = rdif_block::Event::none();
    if let Some(queue_id) = block_queue_ready_from_host_event(host_event) {
        event.push_queue(queue_id);
    }
    debug_assert!(
        host_event.kind() != HostEventKind::None || event.is_empty(),
        "an unhandled SD/MMC source cannot fabricate block queue facts"
    );
    event
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU64;

    use super::*;
    use crate::sdio::HostEventSource;

    #[derive(Clone, Copy, Debug, Default)]
    struct TestEvent(HostEventKind);

    impl HostEvent for TestEvent {
        fn kind(&self) -> HostEventKind {
            self.0
        }

        fn source(&self) -> HostEventSource {
            HostEventSource::Data
        }
    }

    #[test]
    fn error_event_is_routed_before_any_completion_classification() {
        let event = block_event_from_host(&TestEvent(HostEventKind::Error));

        assert!(event.queues().contains(crate::sdio::SDMMC_BLOCK_QUEUE_ID));
    }

    #[test]
    fn masked_source_keeps_nonzero_generation_and_bitmap() {
        let token = rdif_irq::MaskedSource::new(NonZeroU64::MIN, NonZeroU64::MIN);

        assert_eq!(token.generation(), NonZeroU64::MIN);
        assert_eq!(token.bitmap(), NonZeroU64::MIN);
    }

    #[test]
    fn stale_generation_remains_typed_across_the_block_boundary() {
        let error = map_irq_control_error(SdioIrqControlError::StaleGeneration {
            expected: 9,
            actual: 8,
        });

        assert_eq!(
            error,
            rdif_block::IrqControlError::StaleGeneration {
                expected: 9,
                actual: 8,
            }
        );
    }
}
