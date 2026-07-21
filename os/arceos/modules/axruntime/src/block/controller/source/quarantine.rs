//! Bounded retention for IRQ controls whose hardware lifetime is still live.

use ax_kspin::SpinNoPreempt;
use rdif_block::BIrqControl;

const IRQ_SOURCE_QUARANTINE_CAPACITY: usize = 256;

struct QuarantinedIrqSource {
    source_id: usize,
    _control: BIrqControl,
}

enum IrqSourceQuarantineSlot {
    Free,
    Reserved(Option<usize>),
    Occupied(QuarantinedIrqSource),
}

struct IrqSourceQuarantineRegistry {
    slots: [IrqSourceQuarantineSlot; IRQ_SOURCE_QUARANTINE_CAPACITY],
}

impl IrqSourceQuarantineRegistry {
    const fn new() -> Self {
        Self {
            slots: [const { IrqSourceQuarantineSlot::Free }; IRQ_SOURCE_QUARANTINE_CAPACITY],
        }
    }

    fn reserve(&mut self) -> Result<usize, ax_hal::irq::IrqError> {
        let (slot, entry) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| matches!(entry, IrqSourceQuarantineSlot::Free))
            .ok_or(ax_hal::irq::IrqError::NoMemory)?;
        *entry = IrqSourceQuarantineSlot::Reserved(None);
        Ok(slot)
    }

    fn bind(&mut self, slot: usize, source_id: usize) {
        let entry = self
            .slots
            .get_mut(slot)
            .expect("IRQ-source quarantine reservation index is valid");
        assert!(
            matches!(entry, IrqSourceQuarantineSlot::Reserved(None)),
            "IRQ-source quarantine reservation was already bound"
        );
        *entry = IrqSourceQuarantineSlot::Reserved(Some(source_id));
    }

    fn release(&mut self, slot: usize, source_id: Option<usize>) {
        let entry = self
            .slots
            .get_mut(slot)
            .expect("IRQ-source quarantine reservation index is valid");
        assert!(
            matches!(entry, IrqSourceQuarantineSlot::Reserved(bound) if *bound == source_id),
            "IRQ-source quarantine release does not match its reservation"
        );
        *entry = IrqSourceQuarantineSlot::Free;
    }

    fn retain(&mut self, slot: usize, source: QuarantinedIrqSource) -> usize {
        let entry = self
            .slots
            .get_mut(slot)
            .expect("IRQ-source quarantine reservation index is valid");
        assert!(
            matches!(entry, IrqSourceQuarantineSlot::Reserved(Some(bound)) if *bound == source.source_id),
            "IRQ-source quarantine owner does not match its reservation"
        );
        *entry = IrqSourceQuarantineSlot::Occupied(source);
        self.slots
            .iter()
            .filter_map(|entry| match entry {
                IrqSourceQuarantineSlot::Occupied(source) => Some(source.source_id),
                IrqSourceQuarantineSlot::Free | IrqSourceQuarantineSlot::Reserved(_) => None,
            })
            .count()
    }
}

static IRQ_SOURCE_QUARANTINE: SpinNoPreempt<IrqSourceQuarantineRegistry> =
    SpinNoPreempt::new(IrqSourceQuarantineRegistry::new());

pub(super) struct IrqSourceQuarantineReservation {
    slot: Option<usize>,
    source_id: Option<usize>,
}

impl IrqSourceQuarantineReservation {
    pub(super) fn reserve() -> Result<Self, ax_hal::irq::IrqError> {
        let slot = IRQ_SOURCE_QUARANTINE.lock().reserve()?;
        Ok(Self {
            slot: Some(slot),
            source_id: None,
        })
    }

    pub(super) fn bind(mut self, source_id: usize) -> Self {
        let slot = self.slot.expect("live IRQ-source reservation has a slot");
        IRQ_SOURCE_QUARANTINE.lock().bind(slot, source_id);
        self.source_id = Some(source_id);
        self
    }

    pub(super) fn release(mut self) {
        let slot = self
            .slot
            .take()
            .expect("live IRQ-source reservation has a slot");
        IRQ_SOURCE_QUARANTINE.lock().release(slot, self.source_id);
    }

    pub(super) fn retain(mut self, source_id: usize, control: BIrqControl) {
        let slot = self
            .slot
            .take()
            .expect("live IRQ-source reservation has a slot");
        let retained = IRQ_SOURCE_QUARANTINE.lock().retain(
            slot,
            QuarantinedIrqSource {
                source_id,
                _control: control,
            },
        );
        error!("quarantined block IRQ source {source_id}; {retained} source owner(s) retained");
    }
}

impl Drop for IrqSourceQuarantineReservation {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        if self.source_id.is_none() {
            IRQ_SOURCE_QUARANTINE.lock().release(slot, None);
        } else {
            error!(
                "bound block IRQ-source quarantine reservation {} lost its owner",
                self.source_id.unwrap_or(usize::MAX)
            );
        }
    }
}
