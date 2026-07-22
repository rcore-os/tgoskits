//! VM-local interrupt delivery and CPU-interface state transitions.

use alloc::{sync::Arc, vec::Vec};

use super::{ControllerState, GicV3VcpuWake, SpiBacking};
use crate::{
    CpuInterfaceState, GicAffinity, GicVcpuId, IntId, InterruptState, ListRegisterBacking,
    ListRegisterState, LpiId, PhysicalInterruptBinding, QueuedDelivery, RedistributorState,
    SgiTarget, SpiId, VgicError, VgicResult,
};

pub(super) enum DeliveryRetirement {
    Emulated { intid: IntId },
    Physical { binding: PhysicalInterruptBinding },
}

impl ControllerState {
    pub(super) fn redistributor(
        &self,
        vcpu: GicVcpuId,
        operation: &'static str,
    ) -> VgicResult<&RedistributorState> {
        self.redistributors
            .get(&vcpu)
            .ok_or_else(|| VgicError::ResourceNotFound {
                resource: alloc::format!("Redistributor for vCPU {}", vcpu.raw()),
                operation,
            })
    }

    pub(super) fn redistributor_mut(
        &mut self,
        vcpu: GicVcpuId,
        operation: &'static str,
    ) -> VgicResult<&mut RedistributorState> {
        self.redistributors
            .get_mut(&vcpu)
            .ok_or_else(|| VgicError::ResourceNotFound {
                resource: alloc::format!("Redistributor for vCPU {}", vcpu.raw()),
                operation,
            })
    }

    pub(super) fn queue_spi_if_deliverable(
        &mut self,
        spi: SpiId,
    ) -> VgicResult<Option<Arc<dyn GicV3VcpuWake>>> {
        let route = {
            let interrupt = self.distributor.interrupt(spi)?;
            if !self.distributor.enabled() || !interrupt.deliverable() {
                return Ok(None);
            }
            interrupt.route()
        };
        let target = if let Some(route) = route {
            self.redistributors
                .iter()
                .find(|(_, redistributor)| redistributor.affinity() == route)
                .map(|(vcpu, _)| *vcpu)
        } else {
            self.redistributors.keys().next().copied()
        }
        .ok_or_else(|| VgicError::ResourceNotFound {
            resource: alloc::format!("SPI {} target Redistributor", spi.raw()),
            operation: "queue SPI",
        })?;
        let mut canceled_inflight = false;
        for (vcpu, redistributor) in &mut self.redistributors {
            if *vcpu != target {
                canceled_inflight |= redistributor.withdraw_pending_delivery(IntId::Spi(spi));
            }
        }
        if canceled_inflight {
            self.distributor.interrupt_mut(spi)?.cancel_inflight();
        }
        let redistributor = self.redistributor_mut(target, "queue SPI")?;
        redistributor.queue(IntId::Spi(spi));
        Ok(Some(redistributor.wake()))
    }

    pub(super) fn queue_physical_spi(
        &mut self,
        spi: SpiId,
        binding: PhysicalInterruptBinding,
    ) -> VgicResult<Arc<dyn GicV3VcpuWake>> {
        if binding.guest() != IntId::Spi(spi) {
            return Err(VgicError::InvalidConfig {
                detail: alloc::format!(
                    "physical binding for {:?} cannot deliver guest SPI {}",
                    binding.guest(),
                    spi.raw()
                ),
            });
        }
        let distributor_enabled = self.distributor.enabled();
        let interrupt = self.distributor.interrupt_mut(spi)?;
        if !distributor_enabled || !interrupt.enabled() {
            return Err(VgicError::InvalidStateTransition {
                intid: IntId::Spi(spi),
                operation: "forward physical SPI",
                detail: "the guest Distributor or SPI input is disabled".into(),
            });
        }
        interrupt.set_pending(true);
        let redistributor = self.redistributor_mut(binding.target(), "forward physical SPI")?;
        redistributor.queue_physical(IntId::Spi(spi), binding.host());
        Ok(redistributor.wake())
    }

    pub(super) fn queue_local_if_deliverable(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
    ) -> VgicResult<Option<Arc<dyn GicV3VcpuWake>>> {
        let redistributor = self.redistributor_mut(vcpu, "queue local interrupt")?;
        let deliverable = match intid {
            IntId::Sgi(_) | IntId::Ppi(_) => redistributor.private(intid)?.deliverable(),
            IntId::Lpi(lpi) => redistributor
                .lpi(lpi)
                .is_some_and(|interrupt| interrupt.deliverable()),
            IntId::Spi(_) => {
                return Err(VgicError::WrongIntIdClass {
                    intid,
                    operation: "queue Redistributor interrupt",
                });
            }
        };
        if !deliverable {
            return Ok(None);
        }
        redistributor.queue(intid);
        Ok(Some(redistributor.wake()))
    }

    pub(super) fn set_lpi_pending(
        &mut self,
        target: GicVcpuId,
        lpi: LpiId,
        pending: bool,
    ) -> VgicResult<Option<Arc<dyn GicV3VcpuWake>>> {
        let redistributor = self.redistributor_mut(target, "deliver LPI")?;
        let canceled = !pending && redistributor.clear_pending_delivery(IntId::Lpi(lpi));
        let interrupt = redistributor.lpi_mut(lpi);
        interrupt.set_pending(pending);
        if canceled {
            interrupt.cancel_inflight();
        }
        if !pending {
            return Ok(None);
        }
        self.queue_local_if_deliverable(target, IntId::Lpi(lpi))
    }

    pub(super) fn interrupt_state(
        &self,
        vcpu: Option<GicVcpuId>,
        intid: IntId,
    ) -> VgicResult<InterruptState> {
        match intid {
            IntId::Spi(spi) => self.distributor.state(spi),
            IntId::Sgi(_) | IntId::Ppi(_) => Ok(self
                .redistributor(
                    require_vcpu(vcpu, intid, "query private interrupt")?,
                    "query private interrupt",
                )?
                .private(intid)?
                .state()),
            IntId::Lpi(lpi) => self
                .redistributor(require_vcpu(vcpu, intid, "query LPI")?, "query LPI")?
                .lpi(lpi)
                .map(|record| record.state())
                .ok_or_else(|| VgicError::ResourceNotFound {
                    resource: alloc::format!("LPI {}", lpi.raw()),
                    operation: "query LPI",
                }),
        }
    }

    pub(super) fn resolve_sgi_targets(
        &self,
        source: GicVcpuId,
        targets: &SgiTarget,
    ) -> VgicResult<(Vec<GicVcpuId>, Vec<GicAffinity>)> {
        self.redistributor(source, "send SGI")?;
        let selected: Vec<_> = match targets {
            SgiTarget::SelfOnly => self
                .redistributors
                .iter()
                .filter(|(vcpu, _)| **vcpu == source)
                .collect(),
            SgiTarget::AllExceptSelf => self
                .redistributors
                .iter()
                .filter(|(vcpu, _)| **vcpu != source)
                .collect(),
            SgiTarget::Affinities(affinities) => self
                .redistributors
                .iter()
                .filter(|(_, redistributor)| affinities.contains(&redistributor.affinity()))
                .collect(),
        };
        Ok((
            selected.iter().map(|(vcpu, _)| **vcpu).collect(),
            selected
                .iter()
                .map(|(_, redistributor)| redistributor.affinity())
                .collect(),
        ))
    }

    pub(super) fn merge_cpu_interface(
        &mut self,
        vcpu: GicVcpuId,
        saved: CpuInterfaceState,
        refill: bool,
    ) -> VgicResult<Vec<DeliveryRetirement>> {
        let previous = self
            .redistributor(vcpu, "merge CPU interface")?
            .cpu_interface()
            .clone();
        let current_list_registers = saved.list_registers().to_vec();
        self.redistributor_mut(vcpu, "merge CPU interface")?
            .replace_cpu_interface(saved);
        let mut retirements = Vec::new();
        for (index, (old, current)) in previous
            .list_registers()
            .iter()
            .zip(&current_list_registers)
            .enumerate()
        {
            let synchronized = match (old, current) {
                (Some(old), Some(current)) if current.intid() == old.intid() => {
                    if current.backing() != old.backing() {
                        return Err(VgicError::InvalidStateTransition {
                            intid: current.intid(),
                            operation: "synchronize CPU interface",
                            detail: alloc::format!(
                                "list-register backing changed from {:?} to {:?}",
                                old.backing(),
                                current.backing()
                            ),
                        });
                    }
                    Some((
                        current.intid(),
                        self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                    ))
                }
                (Some(old), Some(current)) => {
                    if let Some(retirement) = self.complete_interrupt(vcpu, *old)? {
                        retirements.push(retirement);
                    }
                    Some((
                        current.intid(),
                        self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                    ))
                }
                (Some(old), None) => {
                    if let Some(retirement) = self.complete_interrupt(vcpu, *old)? {
                        retirements.push(retirement);
                    }
                    None
                }
                (None, Some(current)) => Some((
                    current.intid(),
                    self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                )),
                (None, None) => None,
            };
            if let Some((intid, state)) = synchronized {
                self.redistributor_mut(vcpu, "synchronize CPU interface")?
                    .update_list_register_state(index, intid, state)?;
            }
        }
        let eoi_count = self
            .redistributor_mut(vcpu, "consume virtual EOI count")?
            .take_eoi_count();
        for _ in 0..eoi_count {
            let Some(delivery) = self
                .redistributor_mut(vcpu, "consume virtual EOI count")?
                .take_next_active_outside()
            else {
                break;
            };
            if let Some(retirement) = self.deactivate_delivery(vcpu, delivery)? {
                retirements.push(retirement);
            }
        }
        if refill {
            self.refill_cpu_interface(vcpu)?;
        }
        Ok(retirements)
    }

    pub(super) fn refill_cpu_interface(
        &mut self,
        vcpu: GicVcpuId,
    ) -> VgicResult<CpuInterfaceState> {
        let (loaded, snapshot) = {
            let distributor = &self.distributor;
            let redistributor =
                self.redistributors
                    .get_mut(&vcpu)
                    .ok_or_else(|| VgicError::ResourceNotFound {
                        resource: alloc::format!("Redistributor for vCPU {}", vcpu.raw()),
                        operation: "refill CPU interface",
                    })?;
            let outcome = redistributor
                .refill_list_registers(|spi| Ok(distributor.interrupt(spi)?.priority()))?;
            (outcome, redistributor.cpu_interface().clone())
        };
        for intid in loaded.spilled_pending {
            self.cancel_inflight(vcpu, intid)?;
        }
        for intid in loaded.loaded {
            self.mark_inflight(vcpu, intid)?;
        }
        Ok(snapshot)
    }

    fn mark_inflight(&mut self, vcpu: GicVcpuId, intid: IntId) -> VgicResult {
        match intid {
            IntId::Spi(spi) => self.distributor.interrupt_mut(spi)?.mark_inflight(),
            IntId::Sgi(_) | IntId::Ppi(_) => self
                .redistributor_mut(vcpu, "update private interrupt state")?
                .private_mut(intid)?
                .mark_inflight(),
            IntId::Lpi(lpi) => self
                .redistributor_mut(vcpu, "update LPI state")?
                .lpi_mut(lpi)
                .mark_inflight(),
        }
        Ok(())
    }

    fn cancel_inflight(&mut self, vcpu: GicVcpuId, intid: IntId) -> VgicResult {
        match intid {
            IntId::Spi(spi) => self.distributor.interrupt_mut(spi)?.cancel_inflight(),
            IntId::Sgi(_) | IntId::Ppi(_) => self
                .redistributor_mut(vcpu, "spill private interrupt from CPU interface")?
                .private_mut(intid)?
                .cancel_inflight(),
            IntId::Lpi(lpi) => self
                .redistributor_mut(vcpu, "spill LPI from CPU interface")?
                .lpi_mut(lpi)
                .cancel_inflight(),
        }
        Ok(())
    }

    fn synchronize_inflight(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
        state: InterruptState,
    ) -> VgicResult<InterruptState> {
        Ok(match intid {
            IntId::Spi(spi) => self
                .distributor
                .interrupt_mut(spi)?
                .synchronize_inflight(state),
            IntId::Sgi(_) | IntId::Ppi(_) => self
                .redistributor_mut(vcpu, "synchronize private interrupt state")?
                .private_mut(intid)?
                .synchronize_inflight(state),
            IntId::Lpi(lpi) => self
                .redistributor_mut(vcpu, "synchronize LPI state")?
                .lpi_mut(lpi)
                .synchronize_inflight(state),
        })
    }

    fn complete_interrupt(
        &mut self,
        vcpu: GicVcpuId,
        delivery: ListRegisterState,
    ) -> VgicResult<Option<DeliveryRetirement>> {
        let intid = delivery.intid();
        let repend = match intid {
            IntId::Spi(spi) => {
                let interrupt = self.distributor.interrupt_mut(spi)?;
                interrupt.finish_inflight();
                interrupt.deliverable()
            }
            IntId::Sgi(_) | IntId::Ppi(_) => {
                let interrupt = self
                    .redistributor_mut(vcpu, "complete private interrupt")?
                    .private_mut(intid)?;
                interrupt.finish_inflight();
                interrupt.deliverable()
            }
            IntId::Lpi(lpi) => {
                let interrupt = self.redistributor_mut(vcpu, "complete LPI")?.lpi_mut(lpi);
                interrupt.finish_inflight();
                interrupt.deliverable()
            }
        };
        if repend && delivery.backing() == ListRegisterBacking::Software {
            self.redistributor_mut(vcpu, "requeue software interrupt")?
                .queue(intid);
        }
        self.retirement_for(delivery.backing(), intid, false)
    }

    pub(super) fn deactivate_interrupt(
        &mut self,
        vcpu: GicVcpuId,
        intid: IntId,
    ) -> VgicResult<Option<DeliveryRetirement>> {
        let Some(delivery) = self
            .redistributor_mut(vcpu, "deactivate virtual interrupt")?
            .take_active_delivery(intid)
        else {
            return Ok(None);
        };
        self.deactivate_delivery(vcpu, delivery)
    }

    fn deactivate_delivery(
        &mut self,
        vcpu: GicVcpuId,
        delivery: QueuedDelivery,
    ) -> VgicResult<Option<DeliveryRetirement>> {
        let intid = delivery.intid();
        let pending_in_delivery = delivery.state() == InterruptState::ActivePending
            && delivery.backing() == ListRegisterBacking::Software;
        let repend = match intid {
            IntId::Spi(spi) => {
                let interrupt = self.distributor.interrupt_mut(spi)?;
                interrupt.deactivate_inflight(pending_in_delivery);
                interrupt.deliverable()
            }
            IntId::Sgi(_) | IntId::Ppi(_) => {
                let interrupt = self
                    .redistributor_mut(vcpu, "deactivate private interrupt")?
                    .private_mut(intid)?;
                interrupt.deactivate_inflight(pending_in_delivery);
                interrupt.deliverable()
            }
            IntId::Lpi(lpi) => {
                let interrupt = self.redistributor_mut(vcpu, "deactivate LPI")?.lpi_mut(lpi);
                interrupt.deactivate_inflight(pending_in_delivery);
                interrupt.deliverable()
            }
        };
        if repend && delivery.backing() == ListRegisterBacking::Software {
            self.redistributor_mut(vcpu, "requeue deactivated interrupt")?
                .queue(intid);
        }
        self.retirement_for(delivery.backing(), intid, true)
    }

    fn retirement_for(
        &self,
        backing: ListRegisterBacking,
        intid: IntId,
        explicit_deactivation: bool,
    ) -> VgicResult<Option<DeliveryRetirement>> {
        match backing {
            ListRegisterBacking::Software => Ok(Some(DeliveryRetirement::Emulated { intid })),
            ListRegisterBacking::Physical(_) if !explicit_deactivation => Ok(None),
            ListRegisterBacking::Physical(host) => {
                let IntId::Spi(spi) = intid else {
                    return Err(VgicError::WrongIntIdClass {
                        intid,
                        operation: "deactivate physical interrupt",
                    });
                };
                let Some(SpiBacking::Physical(binding)) = self.spi_backings.get(&spi).copied()
                else {
                    return Err(VgicError::InvalidStateTransition {
                        intid,
                        operation: "deactivate physical interrupt",
                        detail: "the hardware-backed LR has no owned physical binding".into(),
                    });
                };
                if binding.host() != host {
                    return Err(VgicError::InvalidStateTransition {
                        intid,
                        operation: "deactivate physical interrupt",
                        detail: alloc::format!(
                            "the LR names host interrupt {}, but ownership names {}",
                            host.raw(),
                            binding.host().raw()
                        ),
                    });
                }
                Ok(Some(DeliveryRetirement::Physical { binding }))
            }
        }
    }
}

fn require_vcpu(
    vcpu: Option<GicVcpuId>,
    intid: IntId,
    operation: &'static str,
) -> VgicResult<GicVcpuId> {
    vcpu.ok_or_else(|| VgicError::InvalidStateTransition {
        intid,
        operation,
        detail: "a vCPU must be specified".into(),
    })
}
