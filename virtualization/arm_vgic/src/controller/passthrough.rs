//! Physical GIC and ITS ownership lifecycle.

use alloc::vec::Vec;

use super::{ControllerInner, ControllerState, GicV3Controller};
use crate::{
    EventId, GicV3Mode, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalInterruptBinding,
    PhysicalIrqId, PhysicalMsiBinding, RedistributorState, SpiId, VgicError, VgicResult,
    backend_result,
};

#[derive(Clone, Copy)]
pub(super) struct PhysicalInterruptEnableSnapshot {
    spi: SpiId,
    binding: PhysicalInterruptBinding,
    enabled: bool,
}

#[derive(Clone, Copy)]
pub(super) struct PhysicalInterruptEnableChange {
    spi: SpiId,
    binding: PhysicalInterruptBinding,
    previous: bool,
    enabled: bool,
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
        Ok(())
    }

    pub(super) fn activate_physical_interrupts(&self, vcpu: GicVcpuId) -> VgicResult {
        let bindings = {
            let mut state = self.inner.state.lock();
            state.redistributor(vcpu, "activate physical interrupts")?;
            if !state.active_vcpus.insert(vcpu) {
                return Err(VgicError::ResourceConflict {
                    resource: "physical interrupt delivery",
                    detail: alloc::format!("vCPU {} is already loaded", vcpu.raw()),
                });
            }
            match state.enabled_physical_interrupts(vcpu) {
                Ok(bindings) => bindings,
                Err(error) => {
                    state.active_vcpus.remove(&vcpu);
                    return Err(error);
                }
            }
        };
        for (activated, binding) in bindings.iter().enumerate() {
            if let Err(error) = self
                .inner
                .backend
                .set_physical_interrupt_enabled(*binding, true)
            {
                for binding in bindings[..activated].iter().rev() {
                    if let Err(rollback_error) = self
                        .inner
                        .backend
                        .set_physical_interrupt_enabled(*binding, false)
                    {
                        log::warn!(
                            "failed to roll back physical interrupt activation for {:?}: \
                             {rollback_error}",
                            binding.host()
                        );
                    }
                }
                self.inner.state.lock().active_vcpus.remove(&vcpu);
                return backend_result(Err(error));
            }
        }
        Ok(())
    }

    pub(super) fn deactivate_physical_interrupts(&self, vcpu: GicVcpuId) -> VgicResult {
        let bindings = {
            let mut state = self.inner.state.lock();
            if !state.active_vcpus.remove(&vcpu) {
                return Ok(());
            }
            state.enabled_physical_interrupts(vcpu)?
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
        match first_error {
            Some(error) => backend_result(Err(error)),
            None => Ok(()),
        }
    }

    pub(super) fn apply_physical_interrupt_enable_changes(
        &self,
        changes: Vec<PhysicalInterruptEnableChange>,
    ) -> VgicResult {
        for (applied, change) in changes.iter().enumerate() {
            if let Err(error) = self
                .inner
                .backend
                .set_physical_interrupt_enabled(change.binding, change.enabled)
            {
                for completed in changes[..applied].iter().rev() {
                    if let Err(rollback_error) = self
                        .inner
                        .backend
                        .set_physical_interrupt_enabled(completed.binding, completed.previous)
                    {
                        log::warn!(
                            "failed to roll back physical interrupt enable state for {:?}: \
                             {rollback_error}",
                            completed.binding.host()
                        );
                    }
                }
                if let Err(rollback_error) = self
                    .inner
                    .state
                    .lock()
                    .restore_physical_interrupt_enable_changes(&changes)
                {
                    log::warn!(
                        "failed to restore GICv3 passthrough enable state after backend error: \
                         {rollback_error}"
                    );
                }
                return backend_result(Err(error));
            }
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
    pub(super) fn physical_interrupt_enable_snapshot(
        &self,
    ) -> VgicResult<Vec<PhysicalInterruptEnableSnapshot>> {
        self.physical_interrupts
            .iter()
            .map(|(spi, binding)| {
                Ok(PhysicalInterruptEnableSnapshot {
                    spi: *spi,
                    binding: *binding,
                    enabled: self.distributor.interrupt(*spi)?.enabled(),
                })
            })
            .collect()
    }

    pub(super) fn active_physical_interrupt_enable_changes(
        &self,
        snapshots: &[PhysicalInterruptEnableSnapshot],
    ) -> VgicResult<Vec<PhysicalInterruptEnableChange>> {
        let mut changes = Vec::new();
        for snapshot in snapshots {
            let enabled = self.distributor.interrupt(snapshot.spi)?.enabled();
            if enabled != snapshot.enabled && self.active_vcpus.contains(&snapshot.binding.target())
            {
                changes.push(PhysicalInterruptEnableChange {
                    spi: snapshot.spi,
                    binding: snapshot.binding,
                    previous: snapshot.enabled,
                    enabled,
                });
            }
        }
        Ok(changes)
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

    fn restore_physical_interrupt_enable_changes(
        &mut self,
        changes: &[PhysicalInterruptEnableChange],
    ) -> VgicResult {
        for change in changes {
            self.distributor
                .interrupt_mut(change.spi)?
                .set_enabled(change.previous);
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
