//! Physical GIC and ITS ownership lifecycle.

use alloc::vec::Vec;

use super::{ControllerInner, ControllerState, GicV3Controller};
use crate::{
    EventId, GicV3Mode, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalInterruptBinding,
    PhysicalInterruptConfiguration, PhysicalIrqId, PhysicalMsiBinding, RedistributorState, SpiId,
    VgicError, VgicResult, backend_result,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PhysicalInterruptState {
    enabled: bool,
    configuration: PhysicalInterruptConfiguration,
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
    /// Binds a guest SPI to an owned physical interrupt and fixed vCPU affinity.
    pub fn bind_physical_spi(
        &self,
        spi: SpiId,
        host: PhysicalIrqId,
        target: GicVcpuId,
    ) -> VgicResult {
        if self.inner.config.mode() != GicV3Mode::Passthrough {
            return Err(VgicError::Unsupported {
                operation: "bind physical SPI",
                detail: "physical bindings require passthrough mode".into(),
            });
        }
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
        let binding = PhysicalInterruptBinding::new(IntId::Spi(spi), host, target, affinity);
        {
            let mut state = self.inner.state.lock();
            if state.physical_interrupts.contains_key(&spi) {
                return Err(VgicError::ResourceConflict {
                    resource: "physical SPI",
                    detail: alloc::format!("guest SPI {} is already bound", spi.raw()),
                });
            }
            if state
                .physical_interrupts
                .values()
                .any(|existing| existing.host() == host)
            {
                return Err(VgicError::ResourceConflict {
                    resource: "physical interrupt",
                    detail: alloc::format!("host interrupt {} is already owned", host.raw()),
                });
            }
            state.physical_interrupts.insert(spi, binding);
        }
        if let Err(error) = backend_result(self.inner.backend.bind_physical_interrupt(binding)) {
            self.inner.state.lock().physical_interrupts.remove(&spi);
            return Err(error);
        }
        if let Err(error) = self
            .inner
            .state
            .lock()
            .distributor
            .claim_passthrough_spi(spi, affinity)
        {
            self.inner.state.lock().physical_interrupts.remove(&spi);
            if let Err(release_error) = self.inner.backend.unbind_physical_interrupt(binding) {
                log::warn!(
                    "failed to release physical interrupt after ownership setup error: \
                     {release_error}"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn activate_physical_interrupts(&self, vcpu: GicVcpuId) -> VgicResult {
        let (interrupts, guest_private) = {
            let state = self.inner.state.lock();
            state.redistributor(vcpu, "activate physical interrupts")?;
            if state.active_vcpus.contains(&vcpu) {
                return Err(VgicError::ResourceConflict {
                    resource: "physical interrupt delivery",
                    detail: alloc::format!("vCPU {} is already loaded", vcpu.raw()),
                });
            }
            (
                state.physical_interrupts_for_vcpu(vcpu)?,
                state
                    .redistributor(vcpu, "snapshot private interrupts before load")?
                    .private_interrupt_state()?,
            )
        };
        let owned = self.inner.config.guest_private_interrupts();
        let host_private = backend_result(self.inner.backend.load_physical_private_interrupts(
            vcpu,
            owned,
            &guest_private,
        ))?;
        for (activated, (binding, state)) in interrupts.iter().enumerate() {
            if let Err(error) = self.activate_physical_interrupt(*binding, *state) {
                self.roll_back_physical_interrupt_activation(&interrupts[..=activated]);
                let mut discarded_guest = guest_private.clone();
                if let Err(rollback_error) = self.inner.backend.save_physical_private_interrupts(
                    vcpu,
                    owned,
                    &mut discarded_guest,
                    &host_private,
                ) {
                    log::warn!(
                        "failed to restore host private interrupts after SPI activation failure: \
                         {rollback_error}"
                    );
                }
                return backend_result(Err(error));
            }
        }
        let mut state = self.inner.state.lock();
        if !state.active_vcpus.insert(vcpu) {
            drop(state);
            let mut discarded_guest = guest_private;
            let _ = self.inner.backend.save_physical_private_interrupts(
                vcpu,
                owned,
                &mut discarded_guest,
                &host_private,
            );
            return Err(VgicError::ResourceConflict {
                resource: "physical interrupt delivery",
                detail: alloc::format!("vCPU {} became loaded concurrently", vcpu.raw()),
            });
        }
        state.private_host_snapshots.insert(vcpu, host_private);
        Ok(())
    }

    fn activate_physical_interrupt(
        &self,
        binding: PhysicalInterruptBinding,
        state: PhysicalInterruptState,
    ) -> Result<(), crate::GicV3BackendError> {
        self.inner
            .backend
            .configure_physical_interrupt(binding, state.configuration)?;
        if state.enabled
            && let Err(error) = self
                .inner
                .backend
                .set_physical_interrupt_enabled(binding, true)
        {
            return Err(error);
        }
        Ok(())
    }

    fn roll_back_physical_interrupt_activation(
        &self,
        interrupts: &[(PhysicalInterruptBinding, PhysicalInterruptState)],
    ) {
        for (binding, state) in interrupts.iter().rev() {
            if state.enabled
                && let Err(error) = self
                    .inner
                    .backend
                    .set_physical_interrupt_enabled(*binding, false)
            {
                log::warn!(
                    "failed to roll back physical interrupt activation for {:?}: {error}",
                    binding.host()
                );
            }
        }
    }

    pub(super) fn deactivate_physical_interrupts(&self, vcpu: GicVcpuId) -> VgicResult {
        let (bindings, mut guest_private, host_private) = {
            let state = self.inner.state.lock();
            if !state.active_vcpus.contains(&vcpu) {
                return Ok(());
            }
            let host_private = state
                .private_host_snapshots
                .get(&vcpu)
                .cloned()
                .ok_or_else(|| VgicError::InvalidConfig {
                    detail: alloc::format!(
                        "vCPU {} is loaded without a saved host private interrupt context",
                        vcpu.raw()
                    ),
                })?;
            (
                state.enabled_physical_interrupts(vcpu)?,
                state
                    .redistributor(vcpu, "snapshot private interrupts before save")?
                    .private_interrupt_state()?,
                host_private,
            )
        };
        let mut first_error = None;
        for binding in bindings {
            if let Err(error) = self
                .inner
                .backend
                .set_physical_interrupt_enabled(binding, false)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        let owned = self.inner.config.guest_private_interrupts();
        if let Err(error) = self.inner.backend.save_physical_private_interrupts(
            vcpu,
            owned,
            &mut guest_private,
            &host_private,
        ) && first_error.is_none()
        {
            first_error = Some(error);
        }
        {
            let mut state = self.inner.state.lock();
            state
                .redistributor_mut(vcpu, "merge saved private interrupts")?
                .merge_private_interrupt_state(&guest_private, owned);
            state.private_host_snapshots.remove(&vcpu);
            state.active_vcpus.remove(&vcpu);
        }
        match first_error {
            Some(error) => backend_result(Err(error)),
            None => Ok(()),
        }
    }

    pub(super) fn synchronize_physical_private_interrupts(&self, vcpu: GicVcpuId) -> VgicResult {
        let mut guest_private = {
            let state = self.inner.state.lock();
            if !state.active_vcpus.contains(&vcpu) {
                return Ok(());
            }
            state
                .redistributor(vcpu, "snapshot private interrupts before synchronization")?
                .private_interrupt_state()?
        };
        let owned = self.inner.config.guest_private_interrupts();
        backend_result(self.inner.backend.synchronize_physical_private_interrupts(
            vcpu,
            owned,
            &mut guest_private,
        ))?;
        self.inner
            .state
            .lock()
            .redistributor_mut(vcpu, "merge synchronized private interrupts")?
            .merge_private_interrupt_state(&guest_private, owned);
        Ok(())
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
                        "failed to restore GICv3 passthrough SPI state after backend error: \
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
        let configuration_changed = change.previous.configuration != change.current.configuration;
        if configuration_changed && change.previous.enabled {
            self.inner
                .backend
                .set_physical_interrupt_enabled(change.binding, false)?;
        }
        if configuration_changed
            && let Err(error) = self
                .inner
                .backend
                .configure_physical_interrupt(change.binding, change.current.configuration)
        {
            if change.previous.enabled {
                let _ = self
                    .inner
                    .backend
                    .set_physical_interrupt_enabled(change.binding, true);
            }
            return Err(error);
        }
        let physical_enabled = change.previous.enabled && !configuration_changed;
        if physical_enabled != change.current.enabled
            && let Err(error) = self
                .inner
                .backend
                .set_physical_interrupt_enabled(change.binding, change.current.enabled)
        {
            if configuration_changed {
                let _ = self
                    .inner
                    .backend
                    .configure_physical_interrupt(change.binding, change.previous.configuration);
            }
            if physical_enabled != change.previous.enabled {
                let _ = self
                    .inner
                    .backend
                    .set_physical_interrupt_enabled(change.binding, change.previous.enabled);
            }
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
        if self.inner.config.mode() != GicV3Mode::Passthrough {
            return Err(VgicError::Unsupported {
                operation: "bind physical MSI",
                detail: "physical ITS bindings require passthrough mode".into(),
            });
        }
        if self.inner.config.its().is_none() {
            return Err(VgicError::Unsupported {
                operation: "bind physical MSI",
                detail: "this controller has no assigned ITS resources".into(),
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
        let binding = PhysicalMsiBinding::new(device, event, lpi, target, affinity);
        {
            let mut state = self.inner.state.lock();
            if state.physical_msi.contains_key(&(device, event)) {
                return Err(VgicError::ResourceConflict {
                    resource: "physical MSI",
                    detail: alloc::format!(
                        "translation ({}, {}) is already bound",
                        device.raw(),
                        event.raw()
                    ),
                });
            }
            if state
                .physical_msi
                .values()
                .any(|existing| existing.lpi() == lpi)
            {
                return Err(VgicError::ResourceConflict {
                    resource: "physical LPI",
                    detail: alloc::format!("LPI {} is already owned", lpi.raw()),
                });
            }
            state.physical_msi.insert((device, event), binding);
        }
        if let Err(error) = backend_result(self.inner.backend.bind_physical_msi(binding)) {
            self.inner
                .state
                .lock()
                .physical_msi
                .remove(&(device, event));
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn physical_binding(
        &self,
        spi: SpiId,
        operation: &'static str,
    ) -> VgicResult<PhysicalInterruptBinding> {
        self.inner
            .state
            .lock()
            .physical_interrupts
            .get(&spi)
            .copied()
            .ok_or_else(|| VgicError::Unsupported {
                operation,
                detail: alloc::format!("SPI {} has no physical binding", spi.raw()),
            })
    }

    pub(super) fn physical_msi_binding(
        &self,
        device: ItsDeviceId,
        event: EventId,
    ) -> VgicResult<PhysicalMsiBinding> {
        self.inner
            .state
            .lock()
            .physical_msi
            .get(&(device, event))
            .copied()
            .ok_or_else(|| VgicError::Unsupported {
                operation: "signal physical MSI",
                detail: alloc::format!(
                    "translation ({}, {}) has no physical ITS binding",
                    device.raw(),
                    event.raw()
                ),
            })
    }
}

impl ControllerState {
    pub(super) fn physical_interrupt_snapshot(&self) -> VgicResult<Vec<PhysicalInterruptSnapshot>> {
        self.physical_interrupts
            .iter()
            .map(|(spi, binding)| {
                Ok(PhysicalInterruptSnapshot {
                    spi: *spi,
                    binding: *binding,
                    state: self.physical_interrupt_state(*spi)?,
                })
            })
            .collect()
    }

    pub(super) fn active_physical_interrupt_state_changes(
        &self,
        snapshots: &[PhysicalInterruptSnapshot],
    ) -> VgicResult<Vec<PhysicalInterruptStateChange>> {
        let mut changes = Vec::new();
        for snapshot in snapshots {
            let current = self.physical_interrupt_state(snapshot.spi)?;
            if current != snapshot.state && self.active_vcpus.contains(&snapshot.binding.target()) {
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
            enabled: interrupt.enabled(),
            configuration: PhysicalInterruptConfiguration::new(
                interrupt.pending(),
                interrupt.active(),
                interrupt.priority(),
                interrupt.trigger(),
            ),
        })
    }

    fn physical_interrupts_for_vcpu(
        &self,
        vcpu: GicVcpuId,
    ) -> VgicResult<Vec<(PhysicalInterruptBinding, PhysicalInterruptState)>> {
        let mut interrupts = Vec::new();
        for (spi, binding) in &self.physical_interrupts {
            if binding.target() == vcpu {
                interrupts.push((*binding, self.physical_interrupt_state(*spi)?));
            }
        }
        Ok(interrupts)
    }

    fn enabled_physical_interrupts(
        &self,
        vcpu: GicVcpuId,
    ) -> VgicResult<Vec<PhysicalInterruptBinding>> {
        let mut bindings = Vec::new();
        for (spi, binding) in &self.physical_interrupts {
            if binding.target() == vcpu && self.distributor.interrupt(*spi)?.enabled() {
                bindings.push(*binding);
            }
        }
        Ok(bindings)
    }

    fn restore_physical_interrupt_state_changes(
        &mut self,
        changes: &[PhysicalInterruptStateChange],
    ) -> VgicResult {
        for change in changes {
            let interrupt = self.distributor.interrupt_mut(change.spi)?;
            interrupt.set_enabled(change.previous.enabled);
            interrupt.set_pending(change.previous.configuration.pending());
            interrupt.set_active(change.previous.configuration.active());
            interrupt.set_priority(change.previous.configuration.priority());
            interrupt.set_trigger(change.previous.configuration.trigger());
        }
        Ok(())
    }
}

impl Drop for ControllerInner {
    fn drop(&mut self) {
        let (interrupts, msi) = {
            let state = self.state.lock();
            (
                state
                    .physical_interrupts
                    .values()
                    .copied()
                    .collect::<Vec<_>>(),
                state.physical_msi.values().copied().collect::<Vec<_>>(),
            )
        };
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
