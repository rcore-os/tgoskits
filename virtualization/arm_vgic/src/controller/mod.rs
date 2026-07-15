//! Per-VM GICv3 controller and its stable delivery API.

mod binding;
mod mmio;
mod passthrough;
mod state;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};

use ax_kspin::SpinRaw;
pub use binding::GicV3VcpuBinding;

use crate::{
    DistributorState, EventId, GicAffinity, GicV3Backend, GicV3Config, GicV3Mode, GicVcpuId,
    GuestMemory, IntId, InterruptState, ItsDeviceId, ItsState, PhysicalInterruptBinding,
    PhysicalMsiBinding, PpiId, RedistributorState, SgiId, SgiTarget, SpiId, TriggerMode, VgicError,
    VgicResult, backend_result,
};

/// Runtime wake capability associated with one attached vCPU.
pub trait GicV3VcpuWake: Send + Sync {
    /// Wakes or kicks the vCPU after an interrupt becomes deliverable.
    fn wake(&self) -> VgicResult;
}

/// One VM-local GICv3 controller.
#[derive(Clone)]
pub struct GicV3Controller {
    inner: Arc<ControllerInner>,
}

impl core::fmt::Debug for GicV3Controller {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("GicV3Controller")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

struct ControllerInner {
    config: GicV3Config,
    backend: Arc<dyn GicV3Backend>,
    guest_memory: Option<Arc<dyn GuestMemory>>,
    state: SpinRaw<ControllerState>,
}

struct ControllerState {
    distributor: DistributorState,
    redistributors: BTreeMap<GicVcpuId, RedistributorState>,
    physical_interrupts: BTreeMap<SpiId, PhysicalInterruptBinding>,
    physical_msi: BTreeMap<(ItsDeviceId, EventId), PhysicalMsiBinding>,
    active_vcpus: BTreeSet<GicVcpuId>,
    its: ItsState,
}

impl GicV3Controller {
    /// Creates a controller with no guest-memory capability.
    pub fn new(config: GicV3Config, backend: Arc<dyn GicV3Backend>) -> VgicResult<Self> {
        Self::new_with_guest_memory(config, backend, None)
    }

    /// Creates a controller with checked guest-memory access for a software ITS.
    pub fn new_with_guest_memory(
        config: GicV3Config,
        backend: Arc<dyn GicV3Backend>,
        guest_memory: Option<Arc<dyn GuestMemory>>,
    ) -> VgicResult<Self> {
        if config.its().is_some() && config.mode() == GicV3Mode::Emulated && guest_memory.is_none()
        {
            return Err(VgicError::InvalidConfig {
                detail: "an emulated ITS requires a guest-memory capability".into(),
            });
        }
        let distributor = DistributorState::new(config.spi_count())?;
        Ok(Self {
            inner: Arc::new(ControllerInner {
                config,
                backend,
                guest_memory,
                state: SpinRaw::new(ControllerState {
                    distributor,
                    redistributors: BTreeMap::new(),
                    physical_interrupts: BTreeMap::new(),
                    physical_msi: BTreeMap::new(),
                    active_vcpus: BTreeSet::new(),
                    its: ItsState::new(),
                }),
            }),
        })
    }

    /// Returns immutable validated configuration.
    pub fn config(&self) -> &GicV3Config {
        &self.inner.config
    }

    /// Attaches one vCPU and returns its lifecycle binding.
    pub fn attach_vcpu(
        &self,
        vcpu: GicVcpuId,
        affinity: GicAffinity,
        wake: Arc<dyn GicV3VcpuWake>,
    ) -> VgicResult<GicV3VcpuBinding> {
        if vcpu.raw() >= self.inner.config.vcpu_count() {
            return Err(VgicError::ResourceNotFound {
                resource: alloc::format!("vCPU {}", vcpu.raw()),
                operation: "attach GICv3 vCPU",
            });
        }
        let mut state = self.inner.state.lock();
        if state.redistributors.contains_key(&vcpu) {
            return Err(VgicError::ResourceConflict {
                resource: "vCPU attachment",
                detail: alloc::format!("vCPU {} is already attached", vcpu.raw()),
            });
        }
        if state
            .redistributors
            .values()
            .any(|redistributor| redistributor.affinity() == affinity)
        {
            return Err(VgicError::ResourceConflict {
                resource: "Redistributor affinity",
                detail: alloc::format!("affinity {affinity:?} is already attached"),
            });
        }
        state.redistributors.insert(
            vcpu,
            RedistributorState::new(
                vcpu,
                affinity,
                self.inner.config.list_register_count(),
                wake,
            )?,
        );
        Ok(GicV3VcpuBinding::new(self.clone(), vcpu))
    }

    /// Validates and records the trigger mode of one software SPI input.
    pub fn configure_spi_input(&self, spi: SpiId, trigger: TriggerMode) -> VgicResult {
        let mut state = self.inner.state.lock();
        if self.inner.config.mode() == GicV3Mode::Passthrough
            && !state.physical_interrupts.contains_key(&spi)
        {
            return Err(VgicError::Unsupported {
                operation: "connect software SPI input",
                detail: alloc::format!(
                    "passthrough SPI {} has no physical interrupt binding",
                    spi.raw()
                ),
            });
        }
        state.distributor.set_trigger(spi, trigger)
    }

    /// Updates the aggregate electrical level of one SPI input.
    pub fn set_spi_level(&self, spi: SpiId, asserted: bool) -> VgicResult {
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            let binding = self.physical_binding(spi, "set SPI level")?;
            return backend_result(
                self.inner
                    .backend
                    .set_physical_interrupt_level(binding, asserted),
            );
        }
        let wake = {
            let mut state = self.inner.state.lock();
            state.distributor.set_level(spi, asserted)?;
            state.queue_spi_if_deliverable(spi)?
        };
        wake_vcpu(wake)
    }

    /// Delivers one edge on an SPI input.
    pub fn pulse_spi(&self, spi: SpiId) -> VgicResult {
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            let binding = self.physical_binding(spi, "pulse SPI")?;
            return backend_result(self.inner.backend.pulse_physical_interrupt(binding));
        }
        let wake = {
            let mut state = self.inner.state.lock();
            state.distributor.pulse(spi)?;
            state.queue_spi_if_deliverable(spi)?
        };
        wake_vcpu(wake)
    }

    /// Updates one vCPU-private PPI input.
    pub fn set_ppi_level(&self, vcpu: GicVcpuId, ppi: PpiId, asserted: bool) -> VgicResult {
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            return Err(VgicError::Unsupported {
                operation: "set software PPI level",
                detail: "passthrough PPIs must be routed by the physical backend".into(),
            });
        }
        let wake = {
            let mut state = self.inner.state.lock();
            state
                .redistributor_mut(vcpu, "set PPI level")?
                .set_ppi_level(ppi, asserted);
            state.queue_local_if_deliverable(vcpu, IntId::Ppi(ppi))?
        };
        wake_vcpu(wake)
    }

    /// Validates and records the trigger mode of one software PPI input.
    pub fn configure_ppi_input(
        &self,
        vcpu: GicVcpuId,
        ppi: PpiId,
        trigger: TriggerMode,
    ) -> VgicResult {
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            return Err(VgicError::Unsupported {
                operation: "connect software PPI input",
                detail: "passthrough PPIs must be routed by the physical backend".into(),
            });
        }
        self.inner
            .state
            .lock()
            .redistributor_mut(vcpu, "configure PPI input")?
            .set_ppi_trigger(ppi, trigger);
        Ok(())
    }

    /// Pulses one vCPU-private PPI input.
    pub fn pulse_ppi(&self, vcpu: GicVcpuId, ppi: PpiId) -> VgicResult {
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            return Err(VgicError::Unsupported {
                operation: "pulse software PPI",
                detail: "passthrough PPIs must be routed by the physical backend".into(),
            });
        }
        let wake = {
            let mut state = self.inner.state.lock();
            state.redistributor_mut(vcpu, "pulse PPI")?.pulse_ppi(ppi);
            state.queue_local_if_deliverable(vcpu, IntId::Ppi(ppi))?
        };
        wake_vcpu(wake)
    }

    /// Sends an SGI using explicit architectural target semantics.
    pub fn send_sgi(&self, source: GicVcpuId, sgi: SgiId, targets: SgiTarget) -> VgicResult {
        let (target_ids, target_affinities) = {
            let state = self.inner.state.lock();
            state.resolve_sgi_targets(source, &targets)?
        };
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            return backend_result(self.inner.backend.send_physical_sgi(
                source,
                sgi,
                &target_affinities,
            ));
        }
        let wakes = {
            let mut state = self.inner.state.lock();
            let mut wakes = Vec::with_capacity(target_ids.len());
            for target in target_ids {
                state.redistributor_mut(target, "send SGI")?.pend_sgi(sgi);
                if let Some(wake) = state.queue_local_if_deliverable(target, IntId::Sgi(sgi))? {
                    wakes.push(wake);
                }
            }
            wakes
        };
        for wake in wakes {
            wake.wake()?;
        }
        Ok(())
    }

    /// Decodes and sends one ICC_SGI1R_EL1 value.
    pub fn write_sgi1r(&self, source: GicVcpuId, value: u64) -> VgicResult {
        let sgi = SgiId::new(((value >> 24) & 0xf) as u8)?;
        if value & (1 << 40) != 0 {
            return self.send_sgi(source, sgi, SgiTarget::AllExceptSelf);
        }
        let aff3 = ((value >> 48) & 0xff) as u8;
        let aff2 = ((value >> 32) & 0xff) as u8;
        let aff1 = ((value >> 16) & 0xff) as u8;
        let range_selector = ((value >> 44) & 0xf) as u8;
        let target_list = value as u16;
        let mut affinities = Vec::new();
        for bit in 0..16u8 {
            if target_list & (1 << bit) != 0 {
                affinities.push(GicAffinity::new(
                    aff3,
                    aff2,
                    aff1,
                    range_selector * 16 + bit,
                ));
            }
        }
        self.send_sgi(source, sgi, SgiTarget::Affinities(affinities))
    }

    /// Validates that a device event can be connected to this controller.
    pub fn configure_msi_input(&self, device: ItsDeviceId, event: EventId) -> VgicResult {
        if self.inner.config.its().is_none() {
            return Err(VgicError::Unsupported {
                operation: "connect MSI input",
                detail: "this controller has no ITS capability".into(),
            });
        }
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            self.physical_msi_binding(device, event)?;
        }
        Ok(())
    }

    /// Signals an MSI through the per-VM ITS translation tables.
    pub fn signal_msi(&self, device: ItsDeviceId, event: EventId) -> VgicResult {
        if self.inner.config.mode() == GicV3Mode::Passthrough {
            let binding = self.physical_msi_binding(device, event)?;
            return backend_result(self.inner.backend.signal_physical_msi(binding));
        }
        let wake = {
            let mut state = self.inner.state.lock();
            let (lpi, target) = state.its.translate(device, event)?;
            state.set_lpi_pending(target, lpi, true)?
        };
        wake_vcpu(wake)
    }

    /// Returns one interrupt's software lifecycle state.
    pub fn interrupt_state(
        &self,
        vcpu: Option<GicVcpuId>,
        intid: IntId,
    ) -> VgicResult<InterruptState> {
        self.inner.state.lock().interrupt_state(vcpu, intid)
    }

    /// Returns the number of pending entries waiting for an LR on one vCPU.
    pub fn software_pending_count(&self, vcpu: GicVcpuId) -> VgicResult<usize> {
        Ok(self
            .inner
            .state
            .lock()
            .redistributor(vcpu, "query pending count")?
            .pending_count())
    }
}

fn wake_vcpu(wake: Option<Arc<dyn GicV3VcpuWake>>) -> VgicResult {
    if let Some(wake) = wake {
        wake.wake()?;
    }
    Ok(())
}
