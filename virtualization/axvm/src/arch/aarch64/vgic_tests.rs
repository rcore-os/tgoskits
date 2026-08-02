use alloc::{sync::Arc, vec::Vec};

use arm_vgic::{ArmSpiRoute, ResidentObservation};

use super::*;

struct FakeIch {
    capacity: usize,
    lrs: [IchLrEntry; MAX_LRS],
    eoi_slots: u16,
    controls: (bool, bool),
}

impl FakeIch {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            lrs: [IchLrEntry::Invalid; MAX_LRS],
            eoi_slots: 0,
            controls: (false, false),
        }
    }
}

impl IchDeliverySession for FakeIch {
    fn lr_capacity(&self) -> usize {
        self.capacity
    }

    fn read_lr(&mut self, slot: usize) -> VmBackendResult<IchLrEntry> {
        self.lrs
            .get(slot)
            .copied()
            .ok_or(VmBackendError::Unsupported)
    }

    fn write_lr(&mut self, slot: usize, entry: IchLrEntry) -> VmBackendResult {
        let lr = self
            .lrs
            .get_mut(slot)
            .filter(|_| slot < self.capacity)
            .ok_or(VmBackendError::Unsupported)?;
        *lr = entry;
        Ok(())
    }

    fn invalidate_lr(&mut self, slot: usize) -> VmBackendResult {
        self.write_lr(slot, IchLrEntry::Invalid)
    }

    fn empty_lr_mask(&mut self) -> VmBackendResult<u16> {
        Ok(self.lrs[..self.capacity]
            .iter()
            .enumerate()
            .fold(0, |mask, (slot, entry)| {
                mask | (u16::from(*entry == IchLrEntry::Invalid) << slot)
            }))
    }

    fn maintenance_eoi_slots(&mut self) -> VmBackendResult<u16> {
        Ok(self.eoi_slots)
    }

    fn set_delivery_controls(&mut self, underflow: bool, trap_dir: bool) -> VmBackendResult {
        self.controls = (underflow, trap_dir);
        Ok(())
    }

    fn deactivate_compatibility_interrupt(
        &mut self,
        intid: ArmVirtualIntId,
    ) -> VmBackendResult<bool> {
        for slot in 0..self.capacity {
            let IchLrEntry::Software {
                intid: resident,
                state,
                priority,
                group1,
                eoi,
            } = self.lrs[slot]
            else {
                continue;
            };
            if resident != intid {
                continue;
            }
            self.lrs[slot] = match state {
                IchLrState::Pending => self.lrs[slot],
                IchLrState::Active => IchLrEntry::Invalid,
                IchLrState::ActivePending => IchLrEntry::Software {
                    intid,
                    state: IchLrState::Pending,
                    priority,
                    group1,
                    eoi,
                },
            };
            return Ok(true);
        }
        Ok(false)
    }
}

fn target(value: u32) -> VgicVcpuId {
    VgicVcpuId::new(value)
}

fn intid(value: u32) -> ArmSpiIntId {
    ArmSpiIntId::new(value).unwrap()
}

fn controller(first: u32, count: u32) -> Arc<ArmVgicController> {
    controller_with_trigger(first, count, InterruptTriggerMode::EdgeTriggered)
}

fn controller_with_trigger(
    first: u32,
    count: u32,
    trigger: InterruptTriggerMode,
) -> Arc<ArmVgicController> {
    let routes: Vec<_> = (first..first + count)
        .map(|raw| (ArmSpiRoute::new(intid(raw), target(0)), trigger))
        .collect();
    Arc::new(ArmVgicController::new(routes).unwrap())
}

fn with_state(entry: IchLrEntry, state: IchLrState) -> IchLrEntry {
    let IchLrEntry::Software {
        intid,
        priority,
        group1,
        eoi,
        ..
    } = entry
    else {
        panic!("expected a software LR")
    };
    IchLrEntry::Software {
        intid,
        state,
        priority,
        group1,
        eoi,
    }
}

fn software_intid(entry: IchLrEntry) -> Option<u32> {
    match entry {
        IchLrEntry::Software { intid, .. } => Some(intid.as_u32()),
        IchLrEntry::Invalid => None,
    }
}

#[test]
fn rejects_a_single_lr_capability() {
    let controller = controller(32, 1);
    controller.pulse(intid(32)).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    assert_eq!(
        delivery.service(&mut FakeIch::new(1), false),
        Err(VmBackendError::Unsupported)
    );
}

#[test]
fn preserves_compatibility_lrs_while_filling_empty_slots() {
    let controller = controller(32, 1);
    controller.pulse(intid(32)).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(2);
    ich.lrs[0] = IchLrEntry::Software {
        intid: ArmVirtualIntId::new(1).unwrap(),
        state: IchLrState::Pending,
        priority: 0,
        group1: true,
        eoi: false,
    };

    delivery.service(&mut ich, false).unwrap();

    assert_eq!(software_intid(ich.lrs[0]), Some(1));
    assert_eq!(software_intid(ich.lrs[1]), Some(32));
    assert_eq!(ich.controls, (false, true));
}

#[test]
fn refills_the_seventeenth_pending_spi_after_an_edge_completion() {
    let controller = controller(32, 17);
    for raw in 32..49 {
        controller.pulse(intid(raw)).unwrap();
    }
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(16);

    delivery.service(&mut ich, false).unwrap();
    assert_eq!(ich.controls, (true, true));
    assert_eq!(software_intid(ich.lrs[0]), Some(32));
    assert_eq!(software_intid(ich.lrs[15]), Some(47));

    ich.lrs[0] = IchLrEntry::Invalid;
    delivery.service(&mut ich, false).unwrap();
    assert_eq!(software_intid(ich.lrs[0]), Some(48));
    assert_eq!(ich.controls, (false, true));
}

#[test]
fn eoimode0_completion_releases_an_edge_lr_without_refill() {
    let controller = controller(32, 1);
    controller.pulse(intid(32)).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(2);
    delivery.service(&mut ich, false).unwrap();

    ich.lrs[0] = IchLrEntry::Invalid;
    delivery.service(&mut ich, false).unwrap();

    assert_eq!(ich.lrs[0], IchLrEntry::Invalid);
    assert_eq!(ich.controls, (false, true));
}

#[test]
fn level_eoi_requeues_only_while_the_input_remains_asserted() {
    let asserted_controller = controller_with_trigger(32, 1, InterruptTriggerMode::LevelTriggered);
    asserted_controller.set_level(intid(32), true).unwrap();
    let mut asserted_delivery = ArmVgicDeliveryPort::new(target(0), asserted_controller.clone());
    let mut asserted_ich = FakeIch::new(2);
    asserted_delivery.service(&mut asserted_ich, false).unwrap();
    asserted_ich.lrs[0] = IchLrEntry::Invalid;
    asserted_ich.eoi_slots = 1;
    asserted_delivery.service(&mut asserted_ich, true).unwrap();
    assert_eq!(software_intid(asserted_ich.lrs[0]), Some(32));

    let lowered_controller = controller_with_trigger(32, 1, InterruptTriggerMode::LevelTriggered);
    lowered_controller.set_level(intid(32), true).unwrap();
    let mut lowered_delivery = ArmVgicDeliveryPort::new(target(0), lowered_controller.clone());
    let mut lowered_ich = FakeIch::new(2);
    lowered_delivery.service(&mut lowered_ich, false).unwrap();
    lowered_controller.set_level(intid(32), false).unwrap();
    lowered_ich.lrs[0] = IchLrEntry::Invalid;
    lowered_ich.eoi_slots = 1;
    lowered_delivery.service(&mut lowered_ich, true).unwrap();
    assert_eq!(lowered_ich.lrs[0], IchLrEntry::Invalid);
}

#[test]
fn lowering_a_level_does_not_evict_an_active_lr() {
    let controller = controller_with_trigger(32, 1, InterruptTriggerMode::LevelTriggered);
    controller.set_level(intid(32), true).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller.clone());
    let mut ich = FakeIch::new(2);
    delivery.service(&mut ich, false).unwrap();
    ich.lrs[0] = with_state(ich.lrs[0], IchLrState::Active);
    delivery.service(&mut ich, false).unwrap();

    controller.set_level(intid(32), false).unwrap();
    delivery.service(&mut ich, false).unwrap();

    assert!(matches!(
        ich.lrs[0],
        IchLrEntry::Software {
            state: IchLrState::Active,
            ..
        }
    ));
}

#[test]
fn delivers_high_architectural_spi_intids() {
    let controller = Arc::new(
        ArmVgicController::new([256, 1019].map(|raw| {
            (
                ArmSpiRoute::new(intid(raw), target(0)),
                InterruptTriggerMode::EdgeTriggered,
            )
        }))
        .unwrap(),
    );
    controller.pulse(intid(256)).unwrap();
    controller.pulse(intid(1019)).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(2);

    delivery.service(&mut ich, false).unwrap();

    assert_eq!(software_intid(ich.lrs[0]), Some(256));
    assert_eq!(software_intid(ich.lrs[1]), Some(1019));
}

#[test]
fn resident_slot_map_stays_aligned_with_restored_raw_lrs() {
    let controller = controller_with_trigger(32, 1, InterruptTriggerMode::LevelTriggered);
    controller.set_level(intid(32), true).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller.clone());
    let mut original = FakeIch::new(2);
    original.lrs[0] = IchLrEntry::Software {
        intid: ArmVirtualIntId::new(1).unwrap(),
        state: IchLrState::Pending,
        priority: 0,
        group1: true,
        eoi: false,
    };
    delivery.service(&mut original, false).unwrap();
    controller.set_level(intid(32), false).unwrap();

    let mut restored = FakeIch::new(2);
    restored.lrs = original.lrs;
    restored.lrs[1] = IchLrEntry::Invalid;
    restored.eoi_slots = 1 << 1;
    delivery.service(&mut restored, true).unwrap();

    assert_eq!(software_intid(restored.lrs[0]), Some(1));
    assert_eq!(restored.lrs[1], IchLrEntry::Invalid);
}

#[test]
fn rejects_eisr_for_an_unmapped_compatibility_slot() {
    let controller = controller(32, 0);
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(2);
    ich.eoi_slots = 1;
    assert_eq!(
        delivery.service(&mut ich, true),
        Err(VmBackendError::InvalidData)
    );
}

#[test]
fn rejects_eisr_for_an_edge_lr_without_eoi_maintenance() {
    let controller = controller(32, 1);
    controller.pulse(intid(32)).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(2);
    delivery.service(&mut ich, false).unwrap();

    ich.lrs[0] = IchLrEntry::Invalid;
    ich.eoi_slots = 1;

    assert_eq!(
        delivery.service(&mut ich, true),
        Err(VmBackendError::InvalidData)
    );
}

#[test]
fn dir_preserves_the_pending_part_of_a_private_active_pending_lr() {
    let controller = controller(32, 0);
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(2);
    let private = ArmVirtualIntId::new(5).unwrap();
    ich.lrs[0] = IchLrEntry::Software {
        intid: private,
        state: IchLrState::ActivePending,
        priority: 0,
        group1: true,
        eoi: false,
    };

    assert_eq!(
        delivery.handle_dir(&mut ich, private).unwrap(),
        DirOutcome::Compatibility
    );
    assert!(matches!(
        ich.lrs[0],
        IchLrEntry::Software {
            state: IchLrState::Pending,
            ..
        }
    ));
}

#[test]
fn dir_deactivates_a_local_module_owned_active_spi() {
    let controller = controller(32, 1);
    controller.pulse(intid(32)).unwrap();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut ich = FakeIch::new(2);
    delivery.service(&mut ich, false).unwrap();
    ich.lrs[0] = with_state(ich.lrs[0], IchLrState::Active);

    assert_eq!(
        delivery
            .handle_dir(&mut ich, ArmVirtualIntId::new(32).unwrap())
            .unwrap(),
        DirOutcome::Completed
    );
    assert_eq!(ich.lrs[0], IchLrEntry::Invalid);
    assert_eq!(ich.controls, (false, true));
}

#[test]
fn dir_preserves_a_remote_module_owner_service_hint() {
    let spi = intid(32);
    let controller = Arc::new(
        ArmVgicController::new([(
            ArmSpiRoute::new(spi, target(1)),
            InterruptTriggerMode::EdgeTriggered,
        )])
        .unwrap(),
    );
    controller.pulse(spi).unwrap();
    let mut descriptor = None;
    controller
        .deliver_one(target(1), |delivery| {
            descriptor = Some(delivery);
            Ok::<_, ()>(())
        })
        .unwrap();
    let descriptor = descriptor.unwrap();
    controller
        .fold(ResidentObservation::new(
            target(1),
            spi,
            descriptor.epoch(),
            ResidentLrState::Active,
            false,
        ))
        .unwrap();
    let controller_for_assertion = controller.clone();
    let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
    let mut local_ich = FakeIch::new(2);
    local_ich.lrs[0] = IchLrEntry::Software {
        intid: ArmVirtualIntId::new(5).unwrap(),
        state: IchLrState::Active,
        priority: 0,
        group1: true,
        eoi: false,
    };
    let local_before = local_ich.lrs;

    assert_eq!(
        delivery.handle_dir(&mut local_ich, ArmVirtualIntId::new(32).unwrap()),
        Ok(DirOutcome::ServiceTarget(target(1)))
    );
    assert_eq!(local_ich.lrs, local_before);
    let mut update = None;
    controller_for_assertion
        .reconcile(
            ResidentObservation::new(
                target(1),
                spi,
                descriptor.epoch(),
                ResidentLrState::Active,
                false,
            ),
            |planned| {
                update = Some(planned);
                Ok::<_, ()>(())
            },
        )
        .unwrap();
    assert_eq!(update, Some(ResidentUpdate::Invalidate));
}
