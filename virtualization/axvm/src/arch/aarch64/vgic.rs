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
    eoi_maintenance: bool,
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
        if !self.eoi_slots_are_valid(capacity, eoi_slots) {
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
                    eoi_maintenance: descriptor.trigger() == InterruptTriggerMode::LevelTriggered,
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

    fn eoi_slots_are_valid(&self, capacity: usize, eoi_slots: u16) -> bool {
        (0..capacity).all(|slot| {
            eoi_slots & (1 << slot) == 0
                || self.slots[slot].is_some_and(|resident| resident.eoi_maintenance)
        })
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
#[path = "vgic_tests.rs"]
mod tests;
