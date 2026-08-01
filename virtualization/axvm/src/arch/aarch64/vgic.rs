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
    ) -> VmBackendResult {
        let Ok(spi) = ArmSpiIntId::new(intid.as_u32()) else {
            session.deactivate_compatibility_interrupt(intid)?;
            return Ok(());
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
            Ok(ServiceHint::Target(_)) => return Err(VmBackendError::Unsupported),
            Err(arm_vgic::VgicError::UnregisteredSpi { .. }) => {
                session.deactivate_compatibility_interrupt(intid)?;
            }
            Err(_) => return Err(VmBackendError::InvalidState),
        }
        self.dir_handler_ready = true;
        self.refill_slots(session, capacity)?;
        self.update_controls(session)
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
        let routes: Vec<_> = (first..first + count)
            .map(|raw| {
                (
                    ArmSpiRoute::new(intid(raw), target(0)),
                    InterruptTriggerMode::EdgeTriggered,
                )
            })
            .collect();
        Arc::new(ArmVgicController::new(routes).unwrap())
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

        delivery.handle_dir(&mut ich, private).unwrap();
        assert!(matches!(
            ich.lrs[0],
            IchLrEntry::Software {
                state: IchLrState::Pending,
                ..
            }
        ));
    }

    #[test]
    fn dir_reports_a_remote_module_owner_as_unsupported() {
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
        let mut delivery = ArmVgicDeliveryPort::new(target(0), controller);

        assert_eq!(
            delivery.handle_dir(&mut FakeIch::new(2), ArmVirtualIntId::new(32).unwrap()),
            Err(VmBackendError::Unsupported)
        );
    }
}
