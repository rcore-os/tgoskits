//! Physical GIC and ITS ownership lifecycle.

use alloc::vec::Vec;

use super::{ControllerInner, GicV3Controller};
use crate::{
    EventId, GicV3Mode, GicVcpuId, IntId, ItsDeviceId, LpiId, PhysicalInterruptBinding,
    PhysicalIrqId, PhysicalMsiBinding, RedistributorState, SpiId, VgicError, VgicResult,
    backend_result,
};

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
