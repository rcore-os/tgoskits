//! Physical GIC and ITS backing lifecycle.

use alloc::vec::Vec;

use axdevice_base::{InterruptTrigger, ItsId};

use super::{ControllerInner, ControllerState, GicV3Controller, MsiBacking, SpiBacking};
use crate::{
    EventId, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalInterruptBinding, PhysicalIrqId,
    PhysicalMsiBinding, RedistributorState, SpiId, VgicError, VgicResult, backend_result,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PhysicalInterruptState {
    distributor_enabled: bool,
    interrupt_enabled: bool,
}

impl PhysicalInterruptState {
    const fn delivery_enabled(self) -> bool {
        self.distributor_enabled && self.interrupt_enabled
    }
}

#[derive(Clone, Copy)]
pub(super) struct PhysicalInterruptSnapshot {
    spi: SpiId,
    binding: PhysicalInterruptBinding,
    state: PhysicalInterruptState,
}

#[derive(Clone, Copy)]
pub(super) struct PhysicalInterruptStateChange {
    spi: SpiId,
    binding: PhysicalInterruptBinding,
    previous: PhysicalInterruptState,
    current: PhysicalInterruptState,
}

impl GicV3Controller {
    /// Queues an acknowledged assigned SPI for hardware-backed LR delivery.
    pub fn forward_physical_spi(&self, spi: SpiId) -> VgicResult {
        let wake = {
            let mut state = self.inner.state.lock();
            let binding = match state.spi_backings.get(&spi).copied() {
                Some(SpiBacking::Physical(binding)) => binding,
                _ => {
                    return Err(VgicError::Unsupported {
                        operation: "forward physical SPI",
                        detail: alloc::format!("SPI {} has no physical binding", spi.raw()),
                    });
                }
            };
            state.queue_physical_spi(spi, binding)?
        };
        if let Some(wake) = wake {
            wake.wake()?;
        }
        Ok(())
    }

    pub(super) fn complete_physical_spi(
        &self,
        vcpu: GicVcpuId,
        binding: PhysicalInterruptBinding,
    ) -> VgicResult {
        backend_result(
            self.inner
                .backend
                .complete_physical_interrupt(vcpu, binding),
        )
    }

    /// Binds a guest SPI to an owned physical interrupt and fixed vCPU affinity.
    pub fn bind_physical_spi(
        &self,
        spi: SpiId,
        host: PhysicalIrqId,
        target: GicVcpuId,
    ) -> VgicResult {
        self.bind_physical_spi_with_trigger(spi, host, target, InterruptTrigger::LevelTriggered)
    }

    /// Binds a guest SPI with its immutable host trigger.
    pub fn bind_physical_spi_with_trigger(
        &self,
        spi: SpiId,
        host: PhysicalIrqId,
        target: GicVcpuId,
        trigger: InterruptTrigger,
    ) -> VgicResult {
        let affinity = {
            let state = self.inner.state.lock();
            state
                .redistributors
                .get(&target)
                .map(RedistributorState::affinity)
                .ok_or_else(|| VgicError::ResourceNotFound {
                    resource: alloc::format!("vCPU {}", target.raw()),
                    operation: "bind physical SPI",
                })?
        };
        let binding =
            PhysicalInterruptBinding::new(IntId::Spi(spi), host, target, affinity, trigger);
        {
            let mut state = self.inner.state.lock();
            if state.spi_backings.contains_key(&spi) {
                return Err(VgicError::ResourceConflict {
                    resource: "GICv3 SPI backing",
                    detail: alloc::format!("guest SPI {} already has a backing", spi.raw()),
                });
            }
            if state
                .spi_backings
                .values()
                .any(|existing| matches!(existing, SpiBacking::Physical(binding) if binding.host() == host))
            {
                return Err(VgicError::ResourceConflict {
                    resource: "physical interrupt",
                    detail: alloc::format!("host interrupt {} is already owned", host.raw()),
                });
            }
            state.distributor.claim_physical_spi(spi, affinity)?;
            if let Err(error) = state
                .distributor
                .set_trigger(spi, crate::core::trigger_mode(trigger))
            {
                if let Err(rollback_error) = state.distributor.release_spi_claim(spi) {
                    log::warn!(
                        "failed to roll back SPI ownership after trigger error: {rollback_error}"
                    );
                }
                return Err(error);
            }
            state
                .spi_backings
                .insert(spi, SpiBacking::Physical(binding));
            state.physical_spi_acknowledged.insert(spi, false);
        }
        if let Err(error) = backend_result(self.inner.backend.bind_physical_interrupt(binding)) {
            let mut state = self.inner.state.lock();
            state.spi_backings.remove(&spi);
            state.physical_spi_acknowledged.remove(&spi);
            if let Err(rollback_error) = state.distributor.release_spi_claim(spi) {
                log::warn!(
                    "failed to roll back SPI ownership after backend error: {rollback_error}"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    /// Releases one quiescent physical SPI binding.
    ///
    /// A loaded vCPU or an acknowledged/in-flight delivery must be completed
    /// before releasing the host source. Backend failure leaves the complete
    /// canonical binding and distributor claim intact so callers can retry.
    pub fn unbind_physical_spi(&self, spi: SpiId) -> VgicResult {
        let binding = {
            let mut state = self.inner.state.lock();
            let binding = match state.spi_backings.get(&spi).copied() {
                Some(SpiBacking::Physical(binding)) => binding,
                _ => {
                    return Err(VgicError::ResourceNotFound {
                        resource: alloc::format!("physical backing for SPI {}", spi.raw()),
                        operation: "unbind physical SPI",
                    });
                }
            };
            state.ensure_physical_spi_is_quiescent(spi, binding)?;
            if !state.releasing_physical_spis.insert(spi) {
                return Err(VgicError::ResourceConflict {
                    resource: "physical interrupt release",
                    detail: alloc::format!("guest SPI {} is already being released", spi.raw()),
                });
            }
            binding
        };

        if let Err(error) = backend_result(self.inner.backend.unbind_physical_interrupt(binding)) {
            self.inner.state.lock().releasing_physical_spis.remove(&spi);
            return Err(error);
        }

        let mut state = self.inner.state.lock();
        state.spi_backings.remove(&spi);
        state.physical_spi_acknowledged.remove(&spi);
        state.releasing_physical_spis.remove(&spi);
        state.distributor.release_spi_claim(spi)
    }

    /// Retires and releases one physical SPI while tearing down a stopped VM.
    ///
    /// Unlike [`Self::unbind_physical_spi`], this operation may discard an
    /// acknowledged or in-flight guest delivery. No vCPU interface may be
    /// loaded. The physical source is masked before canonical state is
    /// retired, and the backend restores host ownership only after any
    /// outstanding activation has been explicitly deactivated.
    ///
    /// If backend unbind fails after deactivation, the binding and claim stay
    /// owned but their delivery state remains quiescent, so retrying this
    /// method never deactivates the same activation twice.
    pub fn teardown_physical_spi(&self, spi: SpiId) -> VgicResult {
        let (binding, needs_deactivation) = {
            let mut state = self.inner.state.lock();
            let binding = match state.spi_backings.get(&spi).copied() {
                Some(SpiBacking::Physical(binding)) => binding,
                _ => {
                    return Err(VgicError::ResourceNotFound {
                        resource: alloc::format!("physical backing for SPI {}", spi.raw()),
                        operation: "tear down physical SPI",
                    });
                }
            };
            if !state.active_vcpus.is_empty() {
                return Err(VgicError::InvalidStateTransition {
                    intid: IntId::Spi(spi),
                    operation: "tear down physical SPI",
                    detail: "one or more virtual CPU interfaces are still loaded".into(),
                });
            }
            if !state.releasing_physical_spis.insert(spi) {
                return Err(VgicError::ResourceConflict {
                    resource: "physical interrupt release",
                    detail: alloc::format!("guest SPI {} is already being released", spi.raw()),
                });
            }
            (binding, state.physical_spi_has_delivery(spi, binding))
        };

        if let Err(error) = backend_result(
            self.inner
                .backend
                .set_physical_interrupt_enabled(binding, false),
        ) {
            self.inner.state.lock().releasing_physical_spis.remove(&spi);
            return Err(error);
        }
        if needs_deactivation
            && let Err(error) = backend_result(
                self.inner
                    .backend
                    .deactivate_physical_interrupt(binding.target(), binding),
            )
        {
            self.inner.state.lock().releasing_physical_spis.remove(&spi);
            return Err(error);
        }

        // There are no loaded vCPUs and `releasing_physical_spis` rejects new
        // forwards, so the canonical delivery cannot change while it is
        // retired. Commit this before unbind: if unbind fails, a retry sees a
        // quiescent binding and must not issue DIR for the same activation.
        self.inner
            .state
            .lock()
            .clear_physical_spi_delivery(spi, binding);

        if let Err(error) = backend_result(self.inner.backend.unbind_physical_interrupt(binding)) {
            self.inner.state.lock().releasing_physical_spis.remove(&spi);
            return Err(error);
        }

        let mut state = self.inner.state.lock();
        state.spi_backings.remove(&spi);
        state.physical_spi_acknowledged.remove(&spi);
        state.releasing_physical_spis.remove(&spi);
        state.distributor.release_spi_claim(spi)
    }

    pub(super) fn apply_physical_interrupt_state_changes(
        &self,
        changes: Vec<PhysicalInterruptStateChange>,
    ) -> VgicResult {
        for (applied, change) in changes.iter().enumerate() {
            if let Err(error) = self.transition_physical_interrupt(change) {
                for completed in changes[..applied].iter().rev() {
                    if let Err(rollback_error) =
                        self.transition_physical_interrupt(&PhysicalInterruptStateChange {
                            spi: completed.spi,
                            binding: completed.binding,
                            previous: completed.current,
                            current: completed.previous,
                        })
                    {
                        log::warn!(
                            "failed to roll back physical interrupt state for {:?}: \
                             {rollback_error}",
                            completed.binding.host()
                        );
                    }
                }
                if let Err(rollback_error) = self
                    .inner
                    .state
                    .lock()
                    .restore_physical_interrupt_state_changes(&changes)
                {
                    log::warn!(
                        "failed to restore GICv3 physical SPI state after backend error: \
                         {rollback_error}"
                    );
                }
                return backend_result(Err(error));
            }
        }
        Ok(())
    }

    fn transition_physical_interrupt(
        &self,
        change: &PhysicalInterruptStateChange,
    ) -> Result<(), crate::GicV3BackendError> {
        let previous = change.previous.delivery_enabled();
        let current = change.current.delivery_enabled();
        if previous != current
            && let Err(error) = self
                .inner
                .backend
                .set_physical_interrupt_enabled(change.binding, current)
        {
            let _ = self
                .inner
                .backend
                .set_physical_interrupt_enabled(change.binding, previous);
            return Err(error);
        }
        Ok(())
    }

    /// Binds one guest MSI translation to VM-owned physical ITS resources.
    pub fn bind_physical_msi(
        &self,
        device: ItsDeviceId,
        event: EventId,
        lpi: LpiId,
        target: GicVcpuId,
    ) -> VgicResult {
        self.bind_physical_msi_for(ItsId::new(0), device, event, lpi, target)
    }

    /// Binds one guest MSI translation in a specific ITS namespace.
    pub fn bind_physical_msi_for(
        &self,
        its: ItsId,
        device: ItsDeviceId,
        event: EventId,
        lpi: LpiId,
        target: GicVcpuId,
    ) -> VgicResult {
        if !self
            .inner
            .config
            .its_instances()
            .iter()
            .any(|(configured, _)| *configured == its)
        {
            return Err(VgicError::Unsupported {
                operation: "bind physical MSI",
                detail: alloc::format!("this controller has no assigned ITS {its:?} resources"),
            });
        }
        if lpi.raw() > self.inner.config.lpi_limit() {
            return Err(VgicError::InvalidIntId { raw: lpi.raw() });
        }
        let affinity = {
            let state = self.inner.state.lock();
            state
                .redistributors
                .get(&target)
                .map(RedistributorState::affinity)
                .ok_or_else(|| VgicError::ResourceNotFound {
                    resource: alloc::format!("vCPU {}", target.raw()),
                    operation: "bind physical MSI",
                })?
        };
        let binding = PhysicalMsiBinding::new(its, device, event, lpi, target, affinity);
        {
            let mut state = self.inner.state.lock();
            if state.msi_backings.contains_key(&(its, device, event)) {
                return Err(VgicError::ResourceConflict {
                    resource: "GICv3 MSI backing",
                    detail: alloc::format!(
                        "MSI event ({}, {}) already has a backing",
                        device.raw(),
                        event.raw()
                    ),
                });
            }
            if state
                .msi_backings
                .values()
                .any(|existing| matches!(existing, MsiBacking::Physical(binding) if binding.lpi() == lpi))
            {
                return Err(VgicError::ResourceConflict {
                    resource: "physical LPI",
                    detail: alloc::format!("LPI {} is already owned", lpi.raw()),
                });
            }
            state
                .msi_backings
                .insert((its, device, event), MsiBacking::Physical(binding));
        }
        if let Err(error) = backend_result(self.inner.backend.bind_physical_msi(binding)) {
            self.inner
                .state
                .lock()
                .msi_backings
                .remove(&(its, device, event));
            return Err(error);
        }
        Ok(())
    }
}

impl ControllerState {
    fn physical_spi_has_delivery(&self, spi: SpiId, binding: PhysicalInterruptBinding) -> bool {
        self.physical_spi_acknowledged
            .get(&spi)
            .copied()
            .unwrap_or(false)
            || self.redistributors.values().any(|redistributor| {
                redistributor.has_physical_delivery(IntId::Spi(spi), binding.host())
            })
            || self
                .distributor
                .interrupt(spi)
                .is_ok_and(crate::InterruptRecord::has_delivery_state)
    }

    fn clear_physical_spi_delivery(&mut self, spi: SpiId, binding: PhysicalInterruptBinding) {
        *self
            .physical_spi_acknowledged
            .get_mut(&spi)
            .expect("an owned physical SPI must have acknowledgement state") = false;
        for redistributor in self.redistributors.values_mut() {
            redistributor.remove_physical_delivery(IntId::Spi(spi), binding.host());
        }
        self.distributor
            .interrupt_mut(spi)
            .expect("an owned physical SPI must have a Distributor record")
            .clear_delivery_state();
    }

    fn ensure_physical_spi_is_quiescent(
        &self,
        spi: SpiId,
        binding: PhysicalInterruptBinding,
    ) -> VgicResult {
        if !self.active_vcpus.is_empty() {
            return Err(VgicError::InvalidStateTransition {
                intid: IntId::Spi(spi),
                operation: "unbind physical SPI",
                detail: "one or more virtual CPU interfaces are still loaded".into(),
            });
        }
        if self.physical_spi_has_delivery(spi, binding) {
            return Err(VgicError::InvalidStateTransition {
                intid: IntId::Spi(spi),
                operation: "unbind physical SPI",
                detail: "an acknowledged or in-flight physical delivery is still pending".into(),
            });
        }
        Ok(())
    }

    pub(super) fn physical_interrupt_snapshot(&self) -> VgicResult<Vec<PhysicalInterruptSnapshot>> {
        self.spi_backings
            .iter()
            .filter_map(|(spi, backing)| match backing {
                SpiBacking::Software => None,
                SpiBacking::Physical(binding) => Some((*spi, *binding)),
            })
            .map(|(spi, binding)| {
                Ok(PhysicalInterruptSnapshot {
                    spi,
                    binding,
                    state: self.physical_interrupt_state(spi)?,
                })
            })
            .collect()
    }

    pub(super) fn physical_interrupt_state_changes(
        &self,
        snapshots: &[PhysicalInterruptSnapshot],
    ) -> VgicResult<Vec<PhysicalInterruptStateChange>> {
        let mut changes = Vec::new();
        for snapshot in snapshots {
            let current = self.physical_interrupt_state(snapshot.spi)?;
            if current.delivery_enabled() != snapshot.state.delivery_enabled() {
                changes.push(PhysicalInterruptStateChange {
                    spi: snapshot.spi,
                    binding: snapshot.binding,
                    previous: snapshot.state,
                    current,
                });
            }
        }
        Ok(changes)
    }

    fn physical_interrupt_state(&self, spi: SpiId) -> VgicResult<PhysicalInterruptState> {
        let interrupt = self.distributor.interrupt(spi)?;
        Ok(PhysicalInterruptState {
            // The host source must stay masked unless both architectural
            // gates exposed to the guest are open. Otherwise a level SPI can
            // enter the host while GICD_CTLR disables guest delivery, fail to
            // acquire a hardware-backed LR, and immediately retrigger.
            distributor_enabled: self.distributor.enabled(),
            interrupt_enabled: interrupt.enabled(),
        })
    }

    fn restore_physical_interrupt_state_changes(
        &mut self,
        changes: &[PhysicalInterruptStateChange],
    ) -> VgicResult {
        for change in changes {
            self.distributor
                .set_enabled_for_rollback(change.previous.distributor_enabled);
            let interrupt = self.distributor.interrupt_mut(change.spi)?;
            interrupt.set_enabled(change.previous.interrupt_enabled);
        }
        Ok(())
    }
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        let (interrupts, retained_interrupts, msi) = {
            let state = self.state.lock();
            (
                state
                    .spi_backings
                    .iter()
                    .filter_map(|(spi, backing)| match backing {
                        SpiBacking::Physical(binding)
                            if state
                                .ensure_physical_spi_is_quiescent(*spi, *binding)
                                .is_ok() =>
                        {
                            Some(*binding)
                        }
                        SpiBacking::Software | SpiBacking::Physical(_) => None,
                    })
                    .collect::<Vec<_>>(),
                state
                    .spi_backings
                    .iter()
                    .filter_map(|(spi, backing)| match backing {
                        SpiBacking::Physical(binding)
                            if state
                                .ensure_physical_spi_is_quiescent(*spi, *binding)
                                .is_err() =>
                        {
                            Some(*binding)
                        }
                        SpiBacking::Software | SpiBacking::Physical(_) => None,
                    })
                    .collect::<Vec<_>>(),
                state
                    .msi_backings
                    .values()
                    .filter_map(|backing| match backing {
                        MsiBacking::Software { .. } => None,
                        MsiBacking::Physical(binding) => Some(*binding),
                    })
                    .collect::<Vec<_>>(),
            )
        };
        for binding in retained_interrupts {
            log::warn!(
                "leaving active physical interrupt {} bound because VGIC teardown was not \
                 completed explicitly",
                binding.host().raw()
            );
        }
        for binding in interrupts {
            if let Err(error) = self.backend.unbind_physical_interrupt(binding) {
                log::warn!("failed to release physical interrupt binding: {error}");
            }
        }
        for binding in msi {
            if let Err(error) = self.backend.unbind_physical_msi(binding) {
                log::warn!("failed to release physical MSI binding: {error}");
            }
        }
    }
}
