//! Target-local LR cache for module-owned virtual SPIs.

use alloc::sync::Arc;

use arm_vcpu::{ArmVirtualIntId, IchLrEntry, IchLrState};
#[cfg(target_arch = "aarch64")]
use arm_vcpu::{IchRuntimeControls, IchSession};
use arm_vgic::{
    ArmSpiIntId, ArmVgicController, DeliveryError, DeliveryOutcome, FoldOutcome, ReconcileError,
    ReconcileOutcome, ResidentLrState, ResidentObservation, ResidentUpdate, ServiceHint,
    VgicVcpuId,
};
use axvm_types::{InterruptTriggerMode, VmBackendError, VmBackendResult};

const MAX_LRS: usize = 16;

pub(super) trait IchDeliverySession {
    fn lr_capacity(&self) -> usize;
    fn read_lr(&mut self, slot: usize) -> VmBackendResult<IchLrEntry>;
    fn write_lr(&mut self, slot: usize, entry: IchLrEntry) -> VmBackendResult;
    fn invalidate_lr(&mut self, slot: usize) -> VmBackendResult;
    fn empty_lr_mask(&mut self) -> VmBackendResult<u16>;
    fn maintenance_eoi_slots(&mut self) -> VmBackendResult<u16>;
    fn set_delivery_controls(&mut self, underflow: bool, trap_dir: bool) -> VmBackendResult;
    fn deactivate_compatibility_interrupt(
        &mut self,
        intid: ArmVirtualIntId,
    ) -> VmBackendResult<bool>;
}

#[cfg(target_arch = "aarch64")]
impl IchDeliverySession for IchSession<'_> {
    fn lr_capacity(&self) -> usize {
        self.capability().list_register_count()
    }

    fn read_lr(&mut self, slot: usize) -> VmBackendResult<IchLrEntry> {
        IchSession::read_lr(self, slot).map_err(|_| VmBackendError::InvalidData)
    }

    fn write_lr(&mut self, slot: usize, entry: IchLrEntry) -> VmBackendResult {
        IchSession::write_lr(self, slot, entry).map_err(|error| match error {
            arm_vcpu::ArmVcpuError::UnsupportedListRegister { .. } => VmBackendError::Unsupported,
            _ => VmBackendError::InvalidState,
        })
    }

    fn invalidate_lr(&mut self, slot: usize) -> VmBackendResult {
        IchSession::invalidate_lr(self, slot).map_err(|_| VmBackendError::InvalidState)
    }

    fn empty_lr_mask(&mut self) -> VmBackendResult<u16> {
        IchSession::empty_lr_mask(self).map_err(|_| VmBackendError::InvalidState)
    }

    fn maintenance_eoi_slots(&mut self) -> VmBackendResult<u16> {
        IchSession::maintenance_snapshot(self)
            .map(|snapshot| snapshot.eoi_slots())
            .map_err(|_| VmBackendError::InvalidData)
    }

    fn set_delivery_controls(&mut self, underflow: bool, trap_dir: bool) -> VmBackendResult {
        let mut controls = IchRuntimeControls::disabled();
        if underflow {
            controls = controls.with_underflow_notification();
        }
        if trap_dir {
            controls = controls.with_trap_deactivation();
        }
        IchSession::set_runtime_controls(self, controls).map_err(|error| match error {
            arm_vcpu::ArmVcpuError::UnsupportedIchHcrPolicy { .. } => VmBackendError::Unsupported,
            _ => VmBackendError::InvalidState,
        })
    }

    fn deactivate_compatibility_interrupt(
        &mut self,
        intid: ArmVirtualIntId,
    ) -> VmBackendResult<bool> {
        IchSession::deactivate_compatibility_interrupt(self, intid)
            .map_err(|_| VmBackendError::InvalidState)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResidentSlot {
    intid: ArmSpiIntId,
    epoch: arm_vgic::DeliveryEpoch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DirOutcome {
    Completed,
    ServiceTarget(VgicVcpuId),
    Compatibility,
}

pub(super) struct ArmVgicDeliveryPort {
    target: VgicVcpuId,
    controller: Arc<ArmVgicController>,
    slots: [Option<ResidentSlot>; MAX_LRS],
    dir_handler_ready: bool,
}

impl ArmVgicDeliveryPort {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "PR3.4 constructs production delivery ports")
    )]
    pub(super) fn new(target: VgicVcpuId, controller: Arc<ArmVgicController>) -> Self {
        Self {
            target,
            controller,
            slots: [None; MAX_LRS],
            dir_handler_ready: true,
        }
    }

    pub(super) fn service(
        &mut self,
        session: &mut impl IchDeliverySession,
        read_maintenance: bool,
    ) -> VmBackendResult {
        let capacity = session.lr_capacity();
        if !(2..=MAX_LRS).contains(&capacity) {
            return Err(VmBackendError::Unsupported);
        }
        let eoi_slots = if read_maintenance {
            session.maintenance_eoi_slots()?
        } else {
            0
        };
        let mapped_slots = self.slots[..capacity]
            .iter()
            .enumerate()
            .fold(0u16, |mask, (slot, resident)| {
                mask | ((resident.is_some() as u16) << slot)
            });
        if eoi_slots & !mapped_slots != 0 {
            return Err(VmBackendError::InvalidData);
        }
        self.fold_slots(session, capacity, eoi_slots)?;
        self.reconcile_slots(session, capacity)?;
        self.refill_slots(session, capacity)?;
        self.update_controls(session)
    }

    pub(super) fn handle_dir(
        &mut self,
        session: &mut impl IchDeliverySession,
        intid: ArmVirtualIntId,
    ) -> VmBackendResult<DirOutcome> {
        let Ok(spi) = ArmSpiIntId::new(intid.as_u32()) else {
            session.deactivate_compatibility_interrupt(intid)?;
            return Ok(DirOutcome::Compatibility);
        };
        let capacity = session.lr_capacity();
        if !(2..=MAX_LRS).contains(&capacity) {
            return Err(VmBackendError::Unsupported);
        }
        self.fold_slots(session, capacity, 0)?;
        match self.controller.request_deactivation(spi) {
            Ok(ServiceHint::None) => {}
            Ok(ServiceHint::Target(owner)) if owner == self.target => {
                self.reconcile_slots(session, capacity)?;
            }
            Ok(ServiceHint::Target(owner)) => {
                self.dir_handler_ready = true;
                self.update_controls(session)?;
                return Ok(DirOutcome::ServiceTarget(owner));
            }
            Err(arm_vgic::VgicError::UnregisteredSpi { .. }) => {
                session.deactivate_compatibility_interrupt(intid)?;
                return Ok(DirOutcome::Compatibility);
            }
            Err(_) => return Err(VmBackendError::InvalidState),
        };
        self.dir_handler_ready = true;
        self.refill_slots(session, capacity)?;
        self.update_controls(session)?;
        Ok(DirOutcome::Completed)
    }

    fn fold_slots(
        &mut self,
        session: &mut impl IchDeliverySession,
        capacity: usize,
        eoi_slots: u16,
    ) -> VmBackendResult {
        for slot in 0..capacity {
            let Some(resident) = self.slots[slot] else {
                continue;
            };
            let state = mapped_lr_state(session.read_lr(slot)?, resident.intid)?;
            let observation = ResidentObservation::new(
                self.target,
                resident.intid,
                resident.epoch,
                state,
                eoi_slots & (1 << slot) != 0,
            );
            match self
                .controller
                .fold(observation)
                .map_err(|_| VmBackendError::InvalidState)?
            {
                FoldOutcome::Resident => {}
                FoldOutcome::Released => {
                    session.invalidate_lr(slot)?;
                    self.slots[slot] = None;
                }
            }
        }
        Ok(())
    }

    fn reconcile_slots(
        &mut self,
        session: &mut impl IchDeliverySession,
        capacity: usize,
    ) -> VmBackendResult {
        for slot in 0..capacity {
            let Some(resident) = self.slots[slot] else {
                continue;
            };
            let entry = session.read_lr(slot)?;
            let state = mapped_lr_state(entry, resident.intid)?;
            let observation =
                ResidentObservation::new(self.target, resident.intid, resident.epoch, state, false);
            let outcome = self.controller.reconcile(observation, |update| {
                apply_update(session, slot, entry, update)
            });
            match outcome {
                Ok(ReconcileOutcome::Resident | ReconcileOutcome::Updated) => {}
                Ok(ReconcileOutcome::Released) => self.slots[slot] = None,
                Err(ReconcileError::Controller(_)) => return Err(VmBackendError::InvalidState),
                Err(ReconcileError::Apply(error)) => return Err(error),
            }
        }
        Ok(())
    }

    fn refill_slots(
        &mut self,
        session: &mut impl IchDeliverySession,
        capacity: usize,
    ) -> VmBackendResult {
        let mut empty = session.empty_lr_mask()?;
        for slot in 0..capacity {
            if self.slots[slot].is_some() || empty & (1 << slot) == 0 {
                continue;
            }
            let mut installed = None;
            let outcome = self.controller.deliver_one(self.target, |descriptor| {
                let intid = ArmVirtualIntId::new(descriptor.intid().as_u32())
                    .map_err(|_| VmBackendError::InvalidData)?;
                let entry = IchLrEntry::Software {
                    intid,
                    state: IchLrState::Pending,
                    priority: 0,
                    group1: true,
                    eoi: descriptor.trigger() == InterruptTriggerMode::LevelTriggered,
                };
                if let Err(error) = session.write_lr(slot, entry) {
                    let _ = session.invalidate_lr(slot);
                    return Err(error);
                }
                installed = Some(ResidentSlot {
                    intid: descriptor.intid(),
                    epoch: descriptor.epoch(),
                });
                Ok(())
            });
            match outcome {
                Ok(DeliveryOutcome::NoWork) => break,
                Ok(DeliveryOutcome::Installed { .. }) => {
                    self.slots[slot] = installed;
                    empty &= !(1 << slot);
                }
                Err(DeliveryError::Controller(_)) => return Err(VmBackendError::InvalidState),
                Err(DeliveryError::Installer(error)) => return Err(error),
            }
        }
        Ok(())
    }

    fn update_controls(&self, session: &mut impl IchDeliverySession) -> VmBackendResult {
        let summary = self
            .controller
            .target_summary(self.target)
            .map_err(|_| VmBackendError::InvalidState)?;
        session.set_delivery_controls(summary.deliverable_outside_lr(), self.dir_handler_ready)
    }
}

fn mapped_lr_state(entry: IchLrEntry, expected: ArmSpiIntId) -> VmBackendResult<ResidentLrState> {
    match entry {
        IchLrEntry::Invalid => Ok(ResidentLrState::Invalid),
        IchLrEntry::Software { intid, state, .. } if intid.as_u32() == expected.as_u32() => {
            Ok(match state {
                IchLrState::Pending => ResidentLrState::Pending,
                IchLrState::Active => ResidentLrState::Active,
                IchLrState::ActivePending => ResidentLrState::ActivePending,
            })
        }
        IchLrEntry::Software { .. } => Err(VmBackendError::InvalidData),
    }
}

fn apply_update(
    session: &mut impl IchDeliverySession,
    slot: usize,
    entry: IchLrEntry,
    update: ResidentUpdate,
) -> VmBackendResult {
    match update {
        ResidentUpdate::Invalidate => session.invalidate_lr(slot),
        ResidentUpdate::SetState(state) => {
            let IchLrEntry::Software {
                intid,
                priority,
                group1,
                eoi,
                ..
            } = entry
            else {
                return Err(VmBackendError::InvalidData);
            };
            let state = match state {
                ResidentLrState::Pending => IchLrState::Pending,
                ResidentLrState::Active => IchLrState::Active,
                ResidentLrState::ActivePending => IchLrState::ActivePending,
                ResidentLrState::Invalid => {
                    return session.invalidate_lr(slot);
                }
            };
            session.write_lr(
                slot,
                IchLrEntry::Software {
                    intid,
                    state,
                    priority,
                    group1,
                    eoi,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
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
    fn refills_the_seventeenth_pending_spi_after_eisr_completion() {
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
        ich.eoi_slots = 1;
        delivery.service(&mut ich, true).unwrap();
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
        ich.eoi_slots = 1;
        delivery.service(&mut ich, true).unwrap();

        assert_eq!(ich.lrs[0], IchLrEntry::Invalid);
        assert_eq!(ich.controls, (false, true));
    }

    #[test]
    fn level_eoi_requeues_only_while_the_input_remains_asserted() {
        let asserted_controller =
            controller_with_trigger(32, 1, InterruptTriggerMode::LevelTriggered);
        asserted_controller.set_level(intid(32), true).unwrap();
        let mut asserted_delivery =
            ArmVgicDeliveryPort::new(target(0), asserted_controller.clone());
        let mut asserted_ich = FakeIch::new(2);
        asserted_delivery.service(&mut asserted_ich, false).unwrap();
        asserted_ich.lrs[0] = IchLrEntry::Invalid;
        asserted_ich.eoi_slots = 1;
        asserted_delivery.service(&mut asserted_ich, true).unwrap();
        assert_eq!(software_intid(asserted_ich.lrs[0]), Some(32));

        let lowered_controller =
            controller_with_trigger(32, 1, InterruptTriggerMode::LevelTriggered);
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
        let controller = controller(32, 1);
        controller.pulse(intid(32)).unwrap();
        let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);
        let mut original = FakeIch::new(2);
        original.lrs[0] = IchLrEntry::Software {
            intid: ArmVirtualIntId::new(1).unwrap(),
            state: IchLrState::Pending,
            priority: 0,
            group1: true,
            eoi: false,
        };
        delivery.service(&mut original, false).unwrap();

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
}
