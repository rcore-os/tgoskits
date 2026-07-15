//! VM-local interrupt delivery and CPU-interface state transitions.

use alloc::{sync::Arc, vec::Vec};

use super::{ControllerState, GicV3VcpuWake};
use crate::{
    CpuInterfaceState, GicAffinity, GicVcpuId, IntId, InterruptState, LpiId, RedistributorState,
    SgiTarget, SpiId, VgicError, VgicResult,
};

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
    ) -> VgicResult<Vec<IntId>> {
        let previous = self
            .redistributor(vcpu, "merge CPU interface")?
            .cpu_interface()
            .clone();
        let current_list_registers = saved.list_registers().to_vec();
        self.redistributor_mut(vcpu, "merge CPU interface")?
            .replace_cpu_interface(saved);
        let mut retired = Vec::new();
        for (index, (old, current)) in previous
            .list_registers()
            .iter()
            .zip(&current_list_registers)
            .enumerate()
        {
            let synchronized = match (old, current) {
                (Some(old), Some(current)) if current.intid() == old.intid() => Some((
                    current.intid(),
                    self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                )),
                (Some(old), Some(current)) => {
                    self.complete_interrupt(vcpu, old.intid())?;
                    retired.push(old.intid());
                    Some((
                        current.intid(),
                        self.synchronize_inflight(vcpu, current.intid(), current.state())?,
                    ))
                }
                (Some(old), None) => {
                    self.complete_interrupt(vcpu, old.intid())?;
                    retired.push(old.intid());
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
        if refill {
            self.refill_cpu_interface(vcpu)?;
        }
        Ok(retired)
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
            let loaded = redistributor
                .refill_list_registers(|spi| Ok(distributor.interrupt(spi)?.priority()))?;
            (loaded, redistributor.cpu_interface().clone())
        };
        for intid in loaded {
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

    fn complete_interrupt(&mut self, vcpu: GicVcpuId, intid: IntId) -> VgicResult {
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
        if repend {
            self.redistributor_mut(vcpu, "requeue level interrupt")?
                .queue(intid);
        }
        Ok(())
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
