//! Physical GIC and ITS backing lifecycle.

use alloc::{sync::Arc, vec::Vec};

use axdevice_base::{InterruptTrigger, ItsId};

use super::{
    ControllerInner, ControllerState, GicV3Native, MsiBacking, SpiBacking,
    state::PhysicalRecordFailure,
};
use crate::{
    EventId, GicV3VcpuWake, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalInterruptBinding,
    PhysicalIrqId, PhysicalMsiBinding, RedistributorState, SpiId, VgicError, VgicResult,
    backend_result,
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

impl GicV3Native {
    /// Records one acknowledged assigned SPI in canonical delivery state.
    ///
    /// This is the record half of physical forwarding: it latches the
    /// architectural pending state and returns the pre-bound wake capability of
    /// the target vCPU. A caller that already holds a raw guard of its own must
    /// use this form and notify the returned capability after releasing that
    /// guard, because the notification sends a deferred kick/IPI. The physical
    /// source identity stays owned by the binding, so the acknowledgement is
    /// never merged into another source by guest vector.
    pub fn acknowledge_physical_spi(
        &self,
        spi: SpiId,
    ) -> Result<Option<Arc<dyn GicV3VcpuWake>>, PhysicalRecordFailure> {
        let mut state = self.inner.state.lock_irqsave();
        let Some(SpiBacking::Physical(binding)) = state.spi_backings.get(&spi).copied() else {
            // The unbound source is a `Copy` failure, so nothing is allocated
            // while raw state is locked. The caller formats it after releasing
            // its own delivery gate.
            return Err(PhysicalRecordFailure::NoBinding { spi });
        };
        state.record_physical_spi(spi, binding)
    }

    /// Records and immediately notifies one acknowledged assigned SPI.
    ///
    /// Convenience for callers that hold no raw guard of their own; the wake is
    /// published after the canonical state lock is released.
    pub fn forward_physical_spi(&self, spi: SpiId) -> VgicResult {
        let wake = self
            .acknowledge_physical_spi(spi)
            .map_err(PhysicalRecordFailure::into_vgic_error)?;
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
            let state = self.inner.state.lock_irqsave();
            state
                .redistributors
                .get(&target)
                .map(RedistributorState::affinity)
                .ok_or(VgicError::NativeState {
                    operation: "bind physical SPI",
                    vcpu: Some(target.raw()),
                    intid: None,
                    reason: "the target vCPU has no attached Redistributor",
                    kind: crate::StateErrorKind::NotFound,
                    detail: crate::NativeStateDetail::None,
                })?
        };
        let binding =
            PhysicalInterruptBinding::new(IntId::Spi(spi), host, target, affinity, trigger);
        {
            let mut state = self.inner.state.lock_irqsave();
            if state.spi_backings.contains_key(&spi) {
                return Err(VgicError::NativeState {
                    operation: "bind physical SPI",
                    vcpu: None,
                    intid: Some(IntId::Spi(spi)),
                    reason: "the guest SPI already has a backing",
                    kind: crate::StateErrorKind::ResourceBusy,
                    detail: crate::NativeStateDetail::None,
                });
            }
            if state
                .spi_backings
                .values()
                .any(|existing| matches!(existing, SpiBacking::Physical(binding) if binding.host() == host))
            {
                return Err(VgicError::NativeState {
                    operation: "bind physical SPI",
                    vcpu: None,
                    intid: Some(IntId::Spi(spi)),
                    reason: "the host interrupt is already owned",
                kind: crate::StateErrorKind::ResourceBusy,
                detail: crate::NativeStateDetail::None,
                });
            }
            state.distributor.claim_physical_spi(spi, affinity)?;
            if let Err(error) = state
                .distributor
                .set_trigger(spi, crate::core::trigger_mode(trigger))
            {
                // Roll the claim back while canonical state is still locked, but
                // release the raw guard before formatting the diagnostic.
                let rollback = state.distributor.release_spi_claim(spi);
                drop(state);
                if let Err(rollback_error) = rollback {
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
            let mut state = self.inner.state.lock_irqsave();
            state.spi_backings.remove(&spi);
            state.physical_spi_acknowledged.remove(&spi);
            // Roll the claim back while canonical state is still locked, but
            // release the raw guard before formatting the diagnostic.
            let rollback = state.distributor.release_spi_claim(spi);
            drop(state);
            if let Err(rollback_error) = rollback {
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
            let mut state = self.inner.state.lock_irqsave();
            let binding = match state.spi_backings.get(&spi).copied() {
                Some(SpiBacking::Physical(binding)) => binding,
                _ => {
                    return Err(VgicError::NativeState {
                        operation: "unbind physical SPI",
                        vcpu: None,
                        intid: Some(IntId::Spi(spi)),
                        reason: "the guest SPI has no physical backing",
                        kind: crate::StateErrorKind::NotFound,
                        detail: crate::NativeStateDetail::None,
                    });
                }
            };
            state.ensure_physical_spi_is_quiescent(spi, binding)?;
            if !state.releasing_physical_spis.insert(spi) {
                return Err(VgicError::NativeState {
                    operation: "unbind physical SPI",
                    vcpu: None,
                    intid: Some(IntId::Spi(spi)),
                    reason: "the guest SPI is already being released",
                    kind: crate::StateErrorKind::ResourceBusy,
                    detail: crate::NativeStateDetail::None,
                });
            }
            binding
        };

        if let Err(error) = backend_result(self.inner.backend.unbind_physical_interrupt(binding)) {
            self.inner
                .state
                .lock_irqsave()
                .releasing_physical_spis
                .remove(&spi);
            return Err(error);
        }

        let mut state = self.inner.state.lock_irqsave();
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
            let mut state = self.inner.state.lock_irqsave();
            let binding = match state.spi_backings.get(&spi).copied() {
                Some(SpiBacking::Physical(binding)) => binding,
                _ => {
                    return Err(VgicError::NativeState {
                        operation: "tear down physical SPI",
                        vcpu: None,
                        intid: Some(IntId::Spi(spi)),
                        reason: "the guest SPI has no physical backing",
                        kind: crate::StateErrorKind::NotFound,
                        detail: crate::NativeStateDetail::None,
                    });
                }
            };
            if state.any_cpu_interface_active() {
                return Err(VgicError::NativeState {
                    operation: "tear down physical SPI",
                    vcpu: None,
                    intid: Some(IntId::Spi(spi)),
                    reason: "one or more virtual CPU interfaces are still loaded",
                    kind: crate::StateErrorKind::InvalidState,
                    detail: crate::NativeStateDetail::None,
                });
            }
            if !state.releasing_physical_spis.insert(spi) {
                return Err(VgicError::NativeState {
                    operation: "tear down physical SPI",
                    vcpu: None,
                    intid: Some(IntId::Spi(spi)),
                    reason: "the guest SPI is already being released",
                    kind: crate::StateErrorKind::ResourceBusy,
                    detail: crate::NativeStateDetail::None,
                });
            }
            (binding, state.physical_spi_has_delivery(spi, binding))
        };

        if let Err(error) = backend_result(
            self.inner
                .backend
                .set_physical_interrupt_enabled(binding, false),
        ) {
            self.inner
                .state
                .lock_irqsave()
                .releasing_physical_spis
                .remove(&spi);
            return Err(error);
        }
        if needs_deactivation
            && let Err(error) = backend_result(
                self.inner
                    .backend
                    .deactivate_physical_interrupt(binding.target(), binding),
            )
        {
            self.inner
                .state
                .lock_irqsave()
                .releasing_physical_spis
                .remove(&spi);
            return Err(error);
        }

        // There are no loaded vCPUs and `releasing_physical_spis` rejects new
        // forwards, so the canonical delivery cannot change while it is
        // retired. Commit this before unbind: if unbind fails, a retry sees a
        // quiescent binding and must not issue DIR for the same activation.
        self.inner
            .state
            .lock_irqsave()
            .clear_physical_spi_delivery(spi, binding);

        if let Err(error) = backend_result(self.inner.backend.unbind_physical_interrupt(binding)) {
            self.inner
                .state
                .lock_irqsave()
                .releasing_physical_spis
                .remove(&spi);
            return Err(error);
        }

        let mut state = self.inner.state.lock_irqsave();
        state.spi_backings.remove(&spi);
        state.physical_spi_acknowledged.remove(&spi);
        state.releasing_physical_spis.remove(&spi);
        state.distributor.release_spi_claim(spi)
    }

    pub(super) fn apply_physical_interrupt_state_changes(
        &self,
        changes: &[PhysicalInterruptStateChange],
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
                // Lock canonical state only for the restore call itself, so the
                // rollback diagnostic is formatted with the raw guard released.
                let restore = self
                    .inner
                    .state
                    .lock_irqsave()
                    .restore_physical_interrupt_state_changes(changes);
                if let Err(rollback_error) = restore {
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
            let state = self.inner.state.lock_irqsave();
            state
                .redistributors
                .get(&target)
                .map(RedistributorState::affinity)
                .ok_or(VgicError::NativeState {
                    operation: "bind physical MSI",
                    vcpu: Some(target.raw()),
                    intid: None,
                    reason: "the target vCPU has no attached Redistributor",
                    kind: crate::StateErrorKind::NotFound,
                    detail: crate::NativeStateDetail::None,
                })?
        };
        let binding = PhysicalMsiBinding::new(its, device, event, lpi, target, affinity);
        // Materialize the LPI record before the raw guard so publishing the
        // binding never grows native storage under the lock.
        self.ensure_lpi_records(target, core::slice::from_ref(&lpi));
        self.with_msi_backings(|backings| {
            if backings.contains_key(&(its, device, event)) {
                return Err(VgicError::NativeState {
                    operation: "bind physical MSI",
                    vcpu: None,
                    intid: Some(IntId::Lpi(lpi)),
                    reason: "the MSI event already has a backing",
                    kind: crate::StateErrorKind::ResourceBusy,
                    detail: crate::NativeStateDetail::None,
                });
            }
            if backings
                .values()
                .any(|existing| matches!(existing, MsiBacking::Physical(binding) if binding.lpi() == lpi))
            {
                return Err(VgicError::NativeState {
                    operation: "bind physical MSI",
                    vcpu: None,
                    intid: Some(IntId::Lpi(lpi)),
                    reason: "the physical LPI is already owned",
                kind: crate::StateErrorKind::ResourceBusy,
                detail: crate::NativeStateDetail::None,
                });
            }
            backings
                .insert((its, device, event), MsiBacking::Physical(binding));
            Ok(())
        })?;
        if let Err(error) = backend_result(self.inner.backend.bind_physical_msi(binding)) {
            self.inner
                .state
                .lock_irqsave()
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
        if self.any_cpu_interface_active() {
            return Err(VgicError::NativeState {
                operation: "unbind physical SPI",
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "one or more virtual CPU interfaces are still loaded",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            });
        }
        if self.physical_spi_has_delivery(spi, binding) {
            return Err(VgicError::NativeState {
                operation: "unbind physical SPI",
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "an acknowledged or in-flight physical delivery is still pending",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
            });
        }
        Ok(())
    }

    /// Fills `out` with the current delivery-gate state of every assigned SPI.
    ///
    /// `out` is reserved by the caller from immutable configuration (one entry
    /// per configured SPI), so this fills preallocated storage instead of
    /// allocating while the canonical raw lock is held.
    pub(super) fn physical_interrupt_snapshot_into(
        &self,
        out: &mut Vec<PhysicalInterruptSnapshot>,
    ) -> VgicResult<()> {
        out.clear();
        for (spi, backing) in self.spi_backings.iter() {
            let SpiBacking::Physical(binding) = backing else {
                continue;
            };
            out.push(PhysicalInterruptSnapshot {
                spi: *spi,
                binding: *binding,
                state: self.physical_interrupt_state(*spi)?,
            });
        }
        Ok(())
    }

    /// Fills `out` with the delivery-gate transitions between `snapshots` and
    /// the current state.
    ///
    /// `out` is reserved by the caller for one entry per assigned SPI.
    pub(super) fn physical_interrupt_state_changes_into(
        &self,
        snapshots: &[PhysicalInterruptSnapshot],
        out: &mut Vec<PhysicalInterruptStateChange>,
    ) -> VgicResult<()> {
        out.clear();
        for snapshot in snapshots {
            let current = self.physical_interrupt_state(snapshot.spi)?;
            if current.delivery_enabled() != snapshot.state.delivery_enabled() {
                out.push(PhysicalInterruptStateChange {
                    spi: snapshot.spi,
                    binding: snapshot.binding,
                    previous: snapshot.state,
                    current,
                });
            }
        }
        Ok(())
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
        let state = self.state.get_mut();
        for (spi, backing) in state.spi_backings.iter() {
            let SpiBacking::Physical(binding) = backing else {
                continue;
            };
            if state
                .ensure_physical_spi_is_quiescent(*spi, *binding)
                .is_err()
            {
                log::warn!(
                    "leaving active physical interrupt {} bound because VGIC teardown was not \
                     completed explicitly",
                    binding.host().raw()
                );
                continue;
            }
            if let Err(error) = self.backend.unbind_physical_interrupt(*binding) {
                log::warn!("failed to release physical interrupt binding: {error}");
            }
        }
        for backing in state.msi_backings.values() {
            if let MsiBacking::Physical(binding) = backing
                && let Err(error) = self.backend.unbind_physical_msi(*binding)
            {
                log::warn!("failed to release physical MSI binding: {error}");
            }
        }
    }
}
