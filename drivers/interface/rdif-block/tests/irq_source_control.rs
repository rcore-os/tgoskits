use rdif_block::{
    BlkError, BlockIrqSource, ContainmentCause, Event, IrqCapture, IrqControlError, IrqEndpoint,
    IrqSourceControl, MaskedSource,
};

struct CapturingEndpoint;

impl IrqEndpoint for CapturingEndpoint {
    type Event = Event;
    type Fault = BlkError;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        IrqCapture::Captured {
            event: Event::from_queue_bits(1 << 3),
            masked: Some(MaskedSource::try_new(9, 1 << 3).unwrap()),
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        Ok(MaskedSource::try_new(10, 1 << 3).unwrap())
    }
}

struct GenerationControl {
    generation: u64,
}

impl IrqSourceControl for GenerationControl {
    type Error = IrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        if source.generation().get() != self.generation {
            return Err(IrqControlError::StaleGeneration {
                expected: self.generation,
                actual: source.generation().get(),
            });
        }
        self.generation += 1;
        Ok(())
    }
}

#[test]
fn block_irq_source_splits_capture_from_owner_side_rearm() {
    let source = BlockIrqSource::new(
        Box::new(CapturingEndpoint),
        Box::new(GenerationControl { generation: 9 }),
    );
    let (mut endpoint, mut control) = source.into_parts();

    let IrqCapture::Captured { event, masked } = endpoint.capture() else {
        panic!("fake source must capture one stable event")
    };
    assert!(event.for_queue(3).is_some());
    let masked = masked.expect("the event must retain device-side mask ownership");
    assert_eq!(control.rearm(masked), Ok(()));
    assert_eq!(
        control.rearm(masked),
        Err(IrqControlError::StaleGeneration {
            expected: 10,
            actual: 9,
        })
    );
}
