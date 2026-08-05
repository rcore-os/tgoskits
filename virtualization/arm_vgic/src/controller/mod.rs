//! Per-VM GICv3 controller and its stable delivery API.

mod binding;
mod mmio;
mod physical;
mod state;
mod v2;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};

use ax_kspin::SpinNoIrq;
use axdevice_base::ItsId;
pub use binding::GicV3VcpuBinding;

use crate::{
    ArmVgicConfig, DistributorState, EventId, GicAffinity, GicV3Backend, GicV3Config,
    GicV3MmioRegion, GicV3SpiOwnership, GicVcpuId, GuestMemory, IntId, InterruptState, ItsDeviceId,
    ItsState, LPI_INTID_MAX, PhysicalInterruptBinding, PhysicalMsiBinding, PpiId,
    RedistributorState, SgiId, SgiTarget, SpiId, TriggerMode, VgicError, VgicResult,
    backend_result,
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

/// Version-neutral canonical VGIC controller used by architecture frontends.
pub type VgicController = GicV3Controller;

impl core::fmt::Debug for GicV3Controller {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("GicV3Controller")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

struct ControllerInner {
    config: ControllerConfig,
    gicv3_config: Option<GicV3Config>,
    backend: Arc<dyn GicV3Backend>,
    guest_memory: Option<Arc<dyn GuestMemory>>,
    // Wired inputs and physical IRQ forwarding can enter from a hard IRQ
    // while the vCPU run path is folding LR state on the same CPU. Saving
    // local IRQ state before taking the canonical state lock prevents that
    // re-entry from spinning on a lock interrupted code already owns.
    state: SpinNoIrq<ControllerState>,
}

#[derive(Clone, Debug)]
pub(crate) struct ControllerConfig {
    spi_ownership: GicV3SpiOwnership,
    distributor_size: u64,
    redistributor_stride: u64,
    vcpu_count: usize,
    its: Vec<(ItsId, GicV3MmioRegion)>,
    spi_count: usize,
    affinity_level_3: bool,
    range_selector: bool,
    lpi_limit: u32,
    list_register_count: usize,
    its_command_budget: usize,
}

impl ControllerConfig {
    fn from_gicv3(config: &GicV3Config) -> Self {
        Self {
            spi_ownership: config.spi_ownership(),
            distributor_size: config.distributor().size(),
            redistributor_stride: config.redistributor_stride(),
            vcpu_count: config.vcpu_count(),
            its: config.its_instances().to_vec(),
            spi_count: config.spi_count(),
            affinity_level_3: config.affinity_level_3(),
            range_selector: config.range_selector(),
            lpi_limit: config.lpi_limit(),
            list_register_count: config.list_register_count(),
            its_command_budget: config.its_command_budget(),
        }
    }

    pub(crate) fn from_arm(config: &ArmVgicConfig) -> VgicResult<Self> {
        match config {
            ArmVgicConfig::V2(config) => Ok(Self {
                spi_ownership: GicV3SpiOwnership::AllGuestOwned,
                distributor_size: config.distributor().size(),
                redistributor_stride: 0x2_0000,
                vcpu_count: config.vcpu_affinities().len(),
                its: Vec::new(),
                spi_count: config.spi_count(),
                affinity_level_3: false,
                range_selector: false,
                lpi_limit: LPI_INTID_MAX,
                list_register_count: config.list_register_count(),
                its_command_budget: 0,
            }),
            ArmVgicConfig::V3(_) => {
                let config = config.internal_gicv3_config()?;
                Ok(Self::from_gicv3(&config))
            }
        }
    }

    pub(crate) const fn spi_ownership(&self) -> GicV3SpiOwnership {
        self.spi_ownership
    }
    pub(crate) const fn distributor_size(&self) -> u64 {
        self.distributor_size
    }
    pub(crate) const fn redistributor_stride(&self) -> u64 {
        self.redistributor_stride
    }
    pub(crate) const fn vcpu_count(&self) -> usize {
        self.vcpu_count
    }
    pub(crate) fn its_instances(&self) -> &[(ItsId, GicV3MmioRegion)] {
        &self.its
    }
    pub(crate) fn its(&self) -> Option<GicV3MmioRegion> {
        self.its.first().map(|(_, region)| *region)
    }
    pub(crate) const fn spi_count(&self) -> usize {
        self.spi_count
    }
    pub(crate) const fn spi_limit(&self) -> u32 {
        32 + self.spi_count as u32
    }
    pub(crate) const fn affinity_level_3(&self) -> bool {
        self.affinity_level_3
    }
    pub(crate) const fn range_selector(&self) -> bool {
        self.range_selector
    }
    pub(crate) const fn lpi_limit(&self) -> u32 {
        self.lpi_limit
    }
    pub(crate) const fn list_register_count(&self) -> usize {
        self.list_register_count
    }
    pub(crate) const fn its_command_budget(&self) -> usize {
        self.its_command_budget
    }
    pub(crate) const fn guest_private_interrupt_mask(&self) -> u32 {
        u32::MAX
    }
    pub(crate) const fn exposes_guest_lpis(&self) -> bool {
        !self.its.is_empty()
    }
}

struct ControllerState {
    distributor: DistributorState,
    redistributors: BTreeMap<GicVcpuId, RedistributorState>,
    spi_backings: BTreeMap<SpiId, SpiBacking>,
    physical_spi_acknowledged: BTreeMap<SpiId, bool>,
    releasing_physical_spis: BTreeSet<SpiId>,
    msi_backings: BTreeMap<(ItsId, ItsDeviceId, EventId), MsiBacking>,
    active_vcpus: alloc::collections::BTreeSet<GicVcpuId>,
    its: BTreeMap<ItsId, ItsState>,
}

#[derive(Clone, Copy)]
enum SpiBacking {
    Software,
    Physical(PhysicalInterruptBinding),
}

#[derive(Clone, Copy)]
enum MsiBacking {
    Software { reserved_lpi: Option<crate::LpiId> },
    Physical(PhysicalMsiBinding),
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
        if !config.its_instances().is_empty() && guest_memory.is_none() {
            return Err(VgicError::InvalidConfig {
                detail: "a guest-visible ITS requires a guest-memory capability".into(),
            });
        }
        let common = ControllerConfig::from_gicv3(&config);
        let distributor = DistributorState::new(common.spi_count())?;
        let its = config
            .its_instances()
            .iter()
            .map(|(id, _)| (*id, ItsState::new()))
            .collect();
        Ok(Self {
            inner: Arc::new(ControllerInner {
                config: common,
                gicv3_config: Some(config),
                backend,
                guest_memory,
                state: SpinNoIrq::new(ControllerState {
                    distributor,
                    redistributors: BTreeMap::new(),
                    spi_backings: BTreeMap::new(),
                    physical_spi_acknowledged: BTreeMap::new(),
                    releasing_physical_spis: BTreeSet::new(),
                    msi_backings: BTreeMap::new(),
                    active_vcpus: alloc::collections::BTreeSet::new(),
                    its,
                }),
            }),
        })
    }

    pub(crate) fn new_from_arm_config(
        config: &ArmVgicConfig,
        backend: Arc<dyn GicV3Backend>,
        guest_memory: Option<Arc<dyn GuestMemory>>,
    ) -> VgicResult<Self> {
        let common = ControllerConfig::from_arm(config)?;
        if !common.its_instances().is_empty() && guest_memory.is_none() {
            return Err(VgicError::InvalidConfig {
                detail: "a guest-visible ITS requires a guest-memory capability".into(),
            });
        }
        let distributor = DistributorState::new(common.spi_count())?;
        let its = common
            .its_instances()
            .iter()
            .map(|(id, _)| (*id, ItsState::new()))
            .collect();
        let gicv3_config = match config {
            ArmVgicConfig::V2(_) => None,
            ArmVgicConfig::V3(_) => Some(config.internal_gicv3_config()?),
        };
        Ok(Self {
            inner: Arc::new(ControllerInner {
                config: common,
                gicv3_config,
                backend,
                guest_memory,
                state: SpinNoIrq::new(ControllerState {
                    distributor,
                    redistributors: BTreeMap::new(),
                    spi_backings: BTreeMap::new(),
                    physical_spi_acknowledged: BTreeMap::new(),
                    releasing_physical_spis: BTreeSet::new(),
                    msi_backings: BTreeMap::new(),
                    active_vcpus: alloc::collections::BTreeSet::new(),
                    its,
                }),
            }),
        })
    }

    /// Returns immutable validated configuration.
    pub fn config(&self) -> &GicV3Config {
        self.inner
            .gicv3_config
            .as_ref()
            .expect("GicV3Controller::config called for a GICv2 VgicCore")
    }

    /// Returns the GICv3 frontend configuration when this controller has one.
    pub fn gicv3_config(&self) -> Option<&GicV3Config> {
        self.inner.gicv3_config.as_ref()
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
                self.inner.config.spi_count(),
                wake,
            )?,
        );
        Ok(GicV3VcpuBinding::new(self.clone(), vcpu))
    }

    /// Validates and records the trigger mode of one software SPI input.
    pub fn configure_spi_input(&self, spi: SpiId, trigger: TriggerMode) -> VgicResult {
        let mut state = self.inner.state.lock();
        state.distributor.interrupt(spi)?;
        match state.spi_backings.get(&spi).copied() {
            Some(SpiBacking::Software) => {}
            Some(SpiBacking::Physical(_)) => {
                return Err(VgicError::ResourceConflict {
                    resource: "GICv3 SPI backing",
                    detail: alloc::format!(
                        "SPI {} is already backed by a physical interrupt",
                        spi.raw()
                    ),
                });
            }
            None => {
                state.distributor.claim_software_spi(spi)?;
                state.spi_backings.insert(spi, SpiBacking::Software);
            }
        }
        state.distributor.set_trigger(spi, trigger)
    }

    /// Updates the aggregate electrical level of one SPI input.
    pub fn set_spi_level(&self, spi: SpiId, asserted: bool) -> VgicResult {
        let wake = {
            let mut state = self.inner.state.lock();
            state.require_software_spi(spi, &self.inner.config, "set SPI level")?;
            state.distributor.set_level(spi, asserted)?;
            if !asserted {
                let mut canceled = false;
                for redistributor in state.redistributors.values_mut() {
                    canceled |= redistributor.withdraw_pending_delivery(IntId::Spi(spi));
                }
                if canceled {
                    state.distributor.interrupt_mut(spi)?.cancel_inflight();
                }
                return Ok(());
            }
            state.queue_spi_if_deliverable(spi)?
        };
        wake_vcpu(wake)
    }

    /// Delivers one edge on an SPI input.
    pub fn pulse_spi(&self, spi: SpiId) -> VgicResult {
        let wake = {
            let mut state = self.inner.state.lock();
            state.require_software_spi(spi, &self.inner.config, "pulse SPI")?;
            state.distributor.pulse(spi)?;
            state.queue_spi_if_deliverable(spi)?
        };
        wake_vcpu(wake)
    }

    /// Updates one vCPU-private PPI input.
    pub fn set_ppi_level(&self, vcpu: GicVcpuId, ppi: PpiId, asserted: bool) -> VgicResult {
        let wake = {
            let mut state = self.inner.state.lock();
            let cpu_interface_loaded = state.active_vcpus.contains(&vcpu);
            state
                .redistributor_mut(vcpu, "set PPI level")?
                .set_ppi_level(ppi, asserted, cpu_interface_loaded);
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
        self.inner
            .state
            .lock()
            .redistributor_mut(vcpu, "configure PPI input")?
            .set_ppi_trigger(ppi, trigger);
        Ok(())
    }

    /// Pulses one vCPU-private PPI input.
    pub fn pulse_ppi(&self, vcpu: GicVcpuId, ppi: PpiId) -> VgicResult {
        let wake = {
            let mut state = self.inner.state.lock();
            state.redistributor_mut(vcpu, "pulse PPI")?.pulse_ppi(ppi);
            state.queue_local_if_deliverable(vcpu, IntId::Ppi(ppi))?
        };
        wake_vcpu(wake)
    }

    /// Sends an SGI using explicit architectural target semantics.
    pub fn send_sgi(&self, source: GicVcpuId, sgi: SgiId, targets: SgiTarget) -> VgicResult {
        let target_ids = {
            let state = self.inner.state.lock();
            state.resolve_sgi_targets(source, &targets)?.0
        };
        let wakes = {
            let mut state = self.inner.state.lock();
            let mut wakes = Vec::with_capacity(target_ids.len());
            for target in target_ids {
                state
                    .redistributor_mut(target, "send SGI")?
                    .pend_sgi(source, sgi);
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
        self.configure_msi_input_for(ItsId::new(0), device, event, None)
    }

    /// Validates and records a planned MSI input in one ITS namespace.
    pub fn configure_msi_input_for(
        &self,
        its: ItsId,
        device: ItsDeviceId,
        event: EventId,
        reserved_lpi: Option<crate::LpiId>,
    ) -> VgicResult {
        if !self
            .inner
            .config
            .its_instances()
            .iter()
            .any(|(configured, _)| *configured == its)
        {
            return Err(VgicError::Unsupported {
                operation: "connect MSI input",
                detail: alloc::format!("this controller has no ITS {its:?} capability"),
            });
        }
        let mut state = self.inner.state.lock();
        match state.msi_backings.get(&(its, device, event)).copied() {
            Some(MsiBacking::Software {
                reserved_lpi: existing,
            }) if existing == reserved_lpi => Ok(()),
            Some(MsiBacking::Software { .. }) => Err(VgicError::ResourceConflict {
                resource: "GICv3 MSI backing",
                detail: alloc::format!(
                    "MSI event ({}, {}, {}) was opened with a different LPI reservation",
                    its.value(),
                    device.raw(),
                    event.raw()
                ),
            }),
            Some(MsiBacking::Physical(_)) => Err(VgicError::ResourceConflict {
                resource: "GICv3 MSI backing",
                detail: alloc::format!(
                    "MSI event ({}, {}) is already backed by a physical translation",
                    device.raw(),
                    event.raw()
                ),
            }),
            None => {
                state
                    .msi_backings
                    .insert((its, device, event), MsiBacking::Software { reserved_lpi });
                Ok(())
            }
        }
    }

    /// Signals an MSI through the per-VM ITS translation tables.
    pub fn signal_msi(&self, device: ItsDeviceId, event: EventId) -> VgicResult {
        self.signal_msi_for(ItsId::new(0), device, event)
    }

    /// Signals one MSI through a specific per-VM ITS.
    pub fn signal_msi_for(&self, its: ItsId, device: ItsDeviceId, event: EventId) -> VgicResult {
        let backing = {
            let state = self.inner.state.lock();
            state.msi_backings.get(&(its, device, event)).copied()
        };
        match backing {
            Some(MsiBacking::Physical(binding)) => {
                return backend_result(self.inner.backend.signal_physical_msi(binding));
            }
            Some(MsiBacking::Software { .. }) => {}
            None => {
                return Err(VgicError::ResourceNotFound {
                    resource: alloc::format!(
                        "MSI input ({}, {}, {})",
                        its.value(),
                        device.raw(),
                        event.raw()
                    ),
                    operation: "signal MSI",
                });
            }
        }
        let wake = {
            let mut state = self.inner.state.lock();
            let (lpi, target) = state
                .its
                .get(&its)
                .ok_or_else(|| VgicError::ResourceNotFound {
                    resource: alloc::format!("ITS {}", its.value()),
                    operation: "signal MSI",
                })?
                .translate(device, event)?;
            if let Some(MsiBacking::Software {
                reserved_lpi: Some(reserved),
            }) = backing
                && reserved != lpi
            {
                return Err(VgicError::ResourceConflict {
                    resource: "planned LPI reservation",
                    detail: alloc::format!(
                        "ITS {} DeviceID {} EventID {} maps LPI {}, but LPI {} is reserved",
                        its.value(),
                        device.raw(),
                        event.raw(),
                        lpi.raw(),
                        reserved.raw()
                    ),
                });
            }
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

    /// Returns whether one vCPU has a pending delivery in or outside its LRs.
    pub fn has_pending_interrupt(&self, vcpu: GicVcpuId) -> VgicResult<bool> {
        Ok(self
            .inner
            .state
            .lock()
            .redistributor(vcpu, "query pending interrupt")?
            .has_pending_delivery())
    }
}

impl ControllerState {
    fn require_software_spi(
        &self,
        spi: SpiId,
        config: &ControllerConfig,
        operation: &'static str,
    ) -> VgicResult {
        self.distributor.interrupt(spi)?;
        match self.spi_backings.get(&spi).copied() {
            Some(SpiBacking::Software) => Ok(()),
            Some(SpiBacking::Physical(_)) => Err(VgicError::Unsupported {
                operation,
                detail: alloc::format!(
                    "SPI {} is electrically driven by its physical backing",
                    spi.raw()
                ),
            }),
            None if config.spi_ownership() == GicV3SpiOwnership::AllGuestOwned => Ok(()),
            None => Err(VgicError::Unsupported {
                operation,
                detail: alloc::format!("SPI {} is not owned by this VM", spi.raw()),
            }),
        }
    }

    fn has_software_backing(&self, spi: SpiId, config: &ControllerConfig) -> bool {
        matches!(self.spi_backings.get(&spi), Some(SpiBacking::Software))
            || (config.spi_ownership() == GicV3SpiOwnership::AllGuestOwned
                && !self.spi_backings.contains_key(&spi))
    }
}

fn wake_vcpu(wake: Option<Arc<dyn GicV3VcpuWake>>) -> VgicResult {
    if let Some(wake) = wake {
        wake.wake()?;
    }
    Ok(())
}
