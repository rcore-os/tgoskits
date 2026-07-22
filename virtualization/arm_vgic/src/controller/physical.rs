//! Physical GIC and ITS backing lifecycle.

use alloc::vec::Vec;

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
        wake.wake()
    }

    /// Binds a guest SPI to an owned physical interrupt and fixed vCPU affinity.
    pub fn bind_physical_spi(
        &self,
        spi: SpiId,
        host: PhysicalIrqId,
        target: GicVcpuId,
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
        let binding = PhysicalInterruptBinding::new(IntId::Spi(spi), host, target, affinity);
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
            state
                .spi_backings
                .insert(spi, SpiBacking::Physical(binding));
        }
        if let Err(error) = backend_result(self.inner.backend.bind_physical_interrupt(binding)) {
            let mut state = self.inner.state.lock();
            state.spi_backings.remove(&spi);
            if let Err(rollback_error) = state.distributor.release_spi_claim(spi) {
                log::warn!(
                    "failed to roll back SPI ownership after backend error: {rollback_error}"
                );
            }
            return Err(error);
        }
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
            if state.msi_backings.contains_key(&(device, event)) {
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
                .insert((device, event), MsiBacking::Physical(binding));
        }
        if let Err(error) = backend_result(self.inner.backend.bind_physical_msi(binding)) {
            self.inner
                .state
                .lock()
                .msi_backings
                .remove(&(device, event));
            return Err(error);
        }
        Ok(())
    }
}

impl ControllerState {
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
        let (interrupts, msi) = {
            let state = self.state.lock();
            (
                state
                    .spi_backings
                    .values()
                    .filter_map(|backing| match backing {
                        SpiBacking::Software => None,
                        SpiBacking::Physical(binding) => Some(*binding),
                    })
                    .collect::<Vec<_>>(),
                state
                    .msi_backings
                    .values()
                    .filter_map(|backing| match backing {
                        MsiBacking::Software => None,
                        MsiBacking::Physical(binding) => Some(*binding),
                    })
                    .collect::<Vec<_>>(),
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
