//! Per-VM GICv3 controller and its stable delivery API.

mod binding;
mod its_service;
mod mmio;
mod physical;
mod slots;
mod state;
mod v2;

use alloc::{sync::Arc, vec::Vec};
use core::ops::Deref;

use ax_sync::RawSpinLock;
use axdevice_base::{InterruptControllerId, ItsId};
pub use binding::GicV3VcpuBinding;
use its_service::ItsService;
use slots::{CanonicalMap, CanonicalSet};
pub use state::PhysicalRecordFailure;

use crate::{
    ArmVgicConfig, DistributorState, EventId, GicAffinity, GicV3Backend, GicV3Config,
    GicV3MmioRegion, GicV3SpiOwnership, GicVcpuId, GuestMemory, IntId, InterruptState, ItsDeviceId,
    LPI_INTID_MAX, LpiId, PhysicalInterruptBinding, PhysicalMsiBinding, PpiId, RedistributorState,
    SgiId, SgiTarget, SpiId, TriggerMode, VgicError, VgicResult, backend_result,
};

/// Runtime wake capability associated with one attached vCPU.
pub trait GicV3VcpuWake: Send + Sync {
    /// Wakes or kicks the vCPU after an interrupt becomes deliverable.
    fn wake(&self) -> VgicResult;
}

/// Native delivery and CPU-interface port.
///
/// Holds only the canonical raw interrupt state, the checked backend, and the
/// immutable configuration. It deliberately contains no guest-memory
/// capability, task-side mutex, resolver map, or full controller. A hardware
/// entry, CPU-interface binding, or wired IRQ sink that a hard IRQ can reach
/// therefore cannot observe a sleepable lock transitively through this type.
#[derive(Clone)]
pub struct GicV3Native {
    inner: Arc<ControllerInner>,
}

/// One VM-local GICv3 controller: the native port plus the task-side software
/// ITS state.
///
/// Only the task-side device graph and control plane hold this value. Hardware
/// facing holders (the architecture entry, CPU-interface bindings, wired IRQ
/// sinks, and vCPU backends) retain a [`GicV3Native`] instead.
#[derive(Clone)]
pub struct GicV3Controller {
    native: GicV3Native,
    its: Arc<ItsService>,
}

impl Deref for GicV3Controller {
    type Target = GicV3Native;

    fn deref(&self) -> &GicV3Native {
        &self.native
    }
}

/// Version-neutral canonical VGIC controller used by architecture frontends.
pub type VgicController = GicV3Controller;

impl core::fmt::Debug for GicV3Native {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("GicV3Native")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Debug for GicV3Controller {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("GicV3Controller")
            .field("config", &self.native.inner.config)
            .finish_non_exhaustive()
    }
}

struct ControllerInner {
    id: InterruptControllerId,
    config: ControllerConfig,
    gicv3_config: Option<GicV3Config>,
    backend: Arc<dyn GicV3Backend>,
    // Wired inputs and physical IRQ forwarding can enter from a hard IRQ
    // while the vCPU run path is folding LR state on the same CPU. Saving
    // local IRQ state before taking the canonical state lock prevents that
    // re-entry from spinning on a lock interrupted code already owns.
    state: RawSpinLock<ControllerState>,
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
    redistributors: CanonicalMap<GicVcpuId, RedistributorState>,
    spi_backings: CanonicalMap<SpiId, SpiBacking>,
    physical_spi_acknowledged: CanonicalMap<SpiId, bool>,
    releasing_physical_spis: CanonicalSet<SpiId>,
    msi_backings: CanonicalMap<(ItsId, ItsDeviceId, EventId), MsiBacking>,
    cpu_interfaces: CpuInterfacePhases,
    /// Preallocated sweep buffer for one CPU-interface refill.
    ///
    /// The refill must snapshot the owned SPIs before it can queue them,
    /// because queueing needs `&mut self`. Reserving the capacity at controller
    /// creation keeps that snapshot allocation-free, and taking the buffer by
    /// value keeps the reserved capacity for the next refill.
    acknowledged_scratch: Vec<SpiId>,
}

/// Fixed per-vCPU CPU-interface phases.
///
/// The array is sized once from immutable configuration, so folding a hardware
/// load or save writes a slot instead of inserting or removing a map entry. The
/// CPU-pinned, IRQ-masked load/save path therefore never allocates or frees
/// while the canonical state lock is held.
struct CpuInterfacePhases {
    slots: Vec<CpuInterfacePhase>,
}

impl CpuInterfacePhases {
    fn new(vcpu_count: usize) -> Self {
        Self {
            slots: alloc::vec![CpuInterfacePhase::Idle; vcpu_count],
        }
    }

    /// Returns one vCPU's phase.
    ///
    /// An id without a Redistributor is `Idle`: `attach_vcpu` rejects ids at or
    /// above the configured vCPU count, so the slots cover every attachable id.
    fn phase(&self, vcpu: GicVcpuId) -> CpuInterfacePhase {
        self.slots
            .get(vcpu.raw())
            .copied()
            .unwrap_or(CpuInterfacePhase::Idle)
    }

    fn set(&mut self, vcpu: GicVcpuId, phase: CpuInterfacePhase) {
        if let Some(slot) = self.slots.get_mut(vcpu.raw()) {
            *slot = phase;
        }
    }

    /// Whether any vCPU still owns a loaded or retiring CPU interface.
    fn any_active(&self) -> bool {
        self.slots
            .iter()
            .any(|phase| *phase != CpuInterfacePhase::Idle)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CpuInterfacePhase {
    /// No CPU interface is loaded for this vCPU.
    Idle,
    Loaded,
    // Hardware state is saved, but lock-free backing retirements still own it.
    Retiring,
}

impl ControllerState {
    fn cpu_interface_phase(&self, vcpu: GicVcpuId) -> CpuInterfacePhase {
        self.cpu_interfaces.phase(vcpu)
    }

    fn set_cpu_interface_phase(&mut self, vcpu: GicVcpuId, phase: CpuInterfacePhase) {
        self.cpu_interfaces.set(vcpu, phase);
    }

    fn cpu_interface_loaded(&self, vcpu: GicVcpuId) -> bool {
        self.cpu_interface_phase(vcpu) == CpuInterfacePhase::Loaded
    }

    /// Whether any vCPU still owns a loaded or retiring CPU interface.
    fn any_cpu_interface_active(&self) -> bool {
        self.cpu_interfaces.any_active()
    }
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

impl GicV3Native {
    /// Builds the native port from already-validated immutable configuration.
    fn new(
        id: InterruptControllerId,
        config: ControllerConfig,
        gicv3_config: Option<GicV3Config>,
        backend: Arc<dyn GicV3Backend>,
    ) -> VgicResult<Self> {
        let distributor = DistributorState::new(config.spi_count())?;
        let cpu_interfaces = CpuInterfacePhases::new(config.vcpu_count());
        let acknowledged_scratch = Vec::with_capacity(config.spi_count());
        let redistributors = CanonicalMap::with_capacity(config.vcpu_count());
        let spi_backings = CanonicalMap::with_capacity(config.spi_count());
        let physical_spi_acknowledged = CanonicalMap::with_capacity(config.spi_count());
        let releasing_physical_spis = CanonicalSet::with_capacity(config.spi_count());
        Ok(Self {
            inner: Arc::new(ControllerInner {
                id,
                config,
                gicv3_config,
                backend,
                state: RawSpinLock::new(ControllerState {
                    distributor,
                    redistributors,
                    spi_backings,
                    physical_spi_acknowledged,
                    releasing_physical_spis,
                    msi_backings: CanonicalMap::with_capacity(0),
                    cpu_interfaces,
                    acknowledged_scratch,
                }),
            }),
        })
    }
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
        let its = Arc::new(ItsService::new(guest_memory, common.its_instances()));
        let native =
            GicV3Native::new(InterruptControllerId::new(0), common, Some(config), backend)?;
        Ok(Self { native, its })
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
        let its = Arc::new(ItsService::new(guest_memory, common.its_instances()));
        let gicv3_config = match config {
            ArmVgicConfig::V2(_) => None,
            ArmVgicConfig::V3(_) => Some(config.internal_gicv3_config()?),
        };
        let native = GicV3Native::new(config.controller_id(), common, gicv3_config, backend)?;
        Ok(Self { native, its })
    }

    /// Returns the native delivery port retained by hardware-facing holders.
    pub fn native_port(&self) -> GicV3Native {
        self.native.clone()
    }
}

impl GicV3Native {
    /// Returns the VM-local controller identifier.
    pub fn id(&self) -> InterruptControllerId {
        self.inner.id
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
        let redistributor = RedistributorState::new(
            vcpu,
            affinity,
            self.inner.config.list_register_count(),
            self.inner.config.spi_count(),
            wake,
        )?;
        let mut state = self.inner.state.lock_irqsave();
        if state.redistributors.contains_key(&vcpu) {
            return Err(VgicError::NativeState {
                operation: "attach GICv3 vCPU",
                vcpu: Some(vcpu.raw()),
                intid: None,
                reason: "the vCPU is already attached",
                kind: crate::StateErrorKind::ResourceBusy,
                detail: crate::NativeStateDetail::None,
            });
        }
        if state
            .redistributors
            .values()
            .any(|redistributor| redistributor.affinity() == affinity)
        {
            return Err(VgicError::NativeState {
                operation: "attach GICv3 vCPU",
                vcpu: Some(vcpu.raw()),
                intid: None,
                reason: "the Redistributor affinity is already attached",
                kind: crate::StateErrorKind::ResourceBusy,
                detail: crate::NativeStateDetail::None,
            });
        }
        state.redistributors.insert(vcpu, redistributor);
        if let Err(error) = state.queue_pending_spis_for_vcpu(vcpu, &self.inner.config) {
            let retired = state.redistributors.remove(&vcpu);
            drop(state);
            drop(retired);
            return Err(error);
        }
        Ok(GicV3VcpuBinding::new(self.clone(), vcpu))
    }

    /// Applies one task-side MSI table transaction with a prepared spare slot.
    /// Native callers only look up entries; growth and retired-buffer release
    /// occur outside the raw guard, even when another configuration races us.
    fn with_msi_backings<T>(
        &self,
        update: impl FnOnce(
            &mut CanonicalMap<(ItsId, ItsDeviceId, EventId), MsiBacking>,
        ) -> VgicResult<T>,
    ) -> VgicResult<T> {
        loop {
            let required =
                {
                    let mut state = self.inner.state.lock_irqsave();
                    if state.msi_backings.entries.len() < state.msi_backings.entries.capacity() {
                        return update(&mut state.msi_backings);
                    }
                    state.msi_backings.entries.len().checked_add(1).ok_or(
                        VgicError::NativeState {
                            operation: "prepare MSI backing slot",
                            vcpu: None,
                            intid: None,
                            reason: "the MSI namespace capacity is exhausted",
                            kind: crate::StateErrorKind::ResourceBusy,
                            detail: crate::NativeStateDetail::None,
                        },
                    )?
                };
            let capacity = required.checked_mul(2).unwrap_or(required);
            let mut prepared = Vec::with_capacity(capacity);
            let mut state = self.inner.state.lock_irqsave();
            if state.msi_backings.entries.len() < state.msi_backings.entries.capacity() {
                drop(state);
                continue;
            }
            if state.msi_backings.entries.len() >= prepared.capacity() {
                drop(state);
                continue;
            }
            prepared.append(&mut state.msi_backings.entries);
            core::mem::swap(&mut prepared, &mut state.msi_backings.entries);
            drop(state);
            drop(prepared);
        }
    }

    /// Materializes LPI records for one vCPU without allocating under the raw
    /// guard.
    ///
    /// Task context only. LPIs become deliverable only after this call, so the
    /// native raw paths never grow the record or delivery storage. A detached
    /// Redistributor is tolerated: the delivery path reports the detached
    /// target once the guard is released.
    pub(crate) fn ensure_lpi_records(&self, vcpu: GicVcpuId, lpis: &[LpiId]) {
        if lpis.is_empty() {
            return;
        }
        loop {
            let (records, queue_capacity, missing) = {
                let state = self.inner.state.lock_irqsave();
                let Some(redistributor) = state.redistributors.get(&vcpu) else {
                    return;
                };
                let missing = lpis
                    .iter()
                    .filter(|lpi| !redistributor.lpi_prepared(**lpi))
                    .count();
                (
                    redistributor.lpi_record_count(),
                    redistributor.delivery_queue_capacity(),
                    missing,
                )
            };
            if missing == 0 {
                return;
            }
            let mut capacity =
                RedistributorState::reserve_lpi_capacity(records, queue_capacity, missing);
            let displaced = {
                let mut state = self.inner.state.lock_irqsave();
                let Some(redistributor) = state.redistributors.get_mut(&vcpu) else {
                    return;
                };
                redistributor.try_install_lpis(&mut capacity, lpis)
            };
            match displaced {
                Some(displaced) => {
                    // Dropping the displaced buffers after the guard is what
                    // keeps the in-guard install allocation- and free-free.
                    drop(displaced);
                    return;
                }
                None => continue,
            }
        }
    }

    /// Validates and records the trigger mode of one software SPI input.
    pub fn configure_spi_input(&self, spi: SpiId, trigger: TriggerMode) -> VgicResult {
        let mut state = self.inner.state.lock_irqsave();
        state.distributor.interrupt(spi)?;
        match state.spi_backings.get(&spi).copied() {
            Some(SpiBacking::Software) => {}
            Some(SpiBacking::Physical(_)) => {
                return Err(VgicError::NativeState {
                    operation: "configure GICv3 SPI input",
                    vcpu: None,
                    intid: Some(IntId::Spi(spi)),
                    reason: "the SPI is already backed by a physical interrupt",
                    kind: crate::StateErrorKind::ResourceBusy,
                    detail: crate::NativeStateDetail::None,
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
            let mut state = self.inner.state.lock_irqsave();
            state.require_software_spi(spi, &self.inner.config, "set SPI level")?;
            state.distributor.set_level(spi, asserted)?;
            if !asserted {
                let mut canceled = false;
                let state = &mut *state;
                let cpu_interfaces = &state.cpu_interfaces;
                for (vcpu, redistributor) in state.redistributors.iter_mut() {
                    let loaded = cpu_interfaces.phase(*vcpu) == CpuInterfacePhase::Loaded;
                    canceled |= redistributor.withdraw_pending_delivery(IntId::Spi(spi), loaded);
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
            let mut state = self.inner.state.lock_irqsave();
            state.require_software_spi(spi, &self.inner.config, "pulse SPI")?;
            state.distributor.pulse(spi)?;
            state.queue_spi_if_deliverable(spi)?
        };
        wake_vcpu(wake)
    }

    /// Updates one vCPU-private PPI input.
    pub fn set_ppi_level(&self, vcpu: GicVcpuId, ppi: PpiId, asserted: bool) -> VgicResult {
        let wake = {
            let mut state = self.inner.state.lock_irqsave();
            let cpu_interface_loaded = state.cpu_interface_loaded(vcpu);
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
            .lock_irqsave()
            .redistributor_mut(vcpu, "configure PPI input")?
            .set_ppi_trigger(ppi, trigger);
        Ok(())
    }

    /// Pulses one vCPU-private PPI input.
    pub fn pulse_ppi(&self, vcpu: GicVcpuId, ppi: PpiId) -> VgicResult {
        let wake = {
            let mut state = self.inner.state.lock_irqsave();
            state.redistributor_mut(vcpu, "pulse PPI")?.pulse_ppi(ppi);
            state.queue_local_if_deliverable(vcpu, IntId::Ppi(ppi))?
        };
        wake_vcpu(wake)
    }

    /// Sends an SGI using explicit architectural target semantics.
    pub fn send_sgi(&self, source: GicVcpuId, sgi: SgiId, targets: SgiTarget) -> VgicResult {
        // Both buffers are bounded by the configured vCPU count and reserved
        // before the raw guard, so the hard-IRQ SGI path never allocates while
        // canonical state is held.
        let vcpu_count = self.inner.config.vcpu_count();
        let mut target_ids: Vec<GicVcpuId> = Vec::with_capacity(vcpu_count);
        let mut wakes: Vec<Arc<dyn GicV3VcpuWake>> = Vec::with_capacity(vcpu_count);
        {
            let state = self.inner.state.lock_irqsave();
            state.resolve_sgi_targets_into(source, &targets, &mut target_ids)?;
        }
        {
            let mut state = self.inner.state.lock_irqsave();
            for target in &target_ids {
                state
                    .redistributor_mut(*target, "send SGI")?
                    .pend_sgi(source, sgi);
                if let Some(wake) = state.queue_local_if_deliverable(*target, IntId::Sgi(sgi))? {
                    wakes.push(wake);
                }
            }
        }
        for wake in &wakes {
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
            return Err(VgicError::NativeState {
                operation: "connect MSI input",
                vcpu: None,
                intid: None,
                reason: "this controller has no ITS capability",
                kind: crate::StateErrorKind::Unsupported,
                detail: crate::NativeStateDetail::None,
            });
        }
        self.with_msi_backings(
            |backings| match backings.get(&(its, device, event)).copied() {
                Some(MsiBacking::Software {
                    reserved_lpi: existing,
                }) if existing == reserved_lpi => Ok(()),
                Some(MsiBacking::Software { .. }) => Err(VgicError::NativeState {
                    operation: "connect MSI input",
                    vcpu: None,
                    intid: None,
                    reason: "the MSI event was opened with a different LPI reservation",
                    kind: crate::StateErrorKind::ResourceBusy,
                    detail: crate::NativeStateDetail::None,
                }),
                Some(MsiBacking::Physical(_)) => Err(VgicError::NativeState {
                    operation: "connect MSI input",
                    vcpu: None,
                    intid: None,
                    reason: "the MSI event is already backed by a physical translation",
                    kind: crate::StateErrorKind::ResourceBusy,
                    detail: crate::NativeStateDetail::None,
                }),
                None => {
                    backings.insert((its, device, event), MsiBacking::Software { reserved_lpi });
                    Ok(())
                }
            },
        )
    }

    /// Returns one interrupt's software lifecycle state.
    pub fn interrupt_state(
        &self,
        vcpu: Option<GicVcpuId>,
        intid: IntId,
    ) -> VgicResult<InterruptState> {
        self.inner.state.lock_irqsave().interrupt_state(vcpu, intid)
    }

    /// Returns the number of pending entries waiting for an LR on one vCPU.
    pub fn software_pending_count(&self, vcpu: GicVcpuId) -> VgicResult<usize> {
        Ok(self
            .inner
            .state
            .lock_irqsave()
            .redistributor(vcpu, "query pending count")?
            .pending_count())
    }

    /// Returns whether one vCPU has a pending delivery in or outside its LRs.
    pub fn has_pending_interrupt(&self, vcpu: GicVcpuId) -> VgicResult<bool> {
        Ok(self
            .inner
            .state
            .lock_irqsave()
            .redistributor(vcpu, "query pending interrupt")?
            .has_pending_delivery())
    }
}

impl GicV3Controller {
    /// Signals an MSI through the per-VM ITS translation tables.
    pub fn signal_msi(&self, device: ItsDeviceId, event: EventId) -> VgicResult {
        self.signal_msi_for(ItsId::new(0), device, event)
    }

    /// Signals one MSI through a specific per-VM ITS.
    ///
    /// The ITS translation is resolved under the sleepable ITS mutex, outside
    /// the native raw lock. Only the resulting short pending update re-enters
    /// the native state, and the wake is published after every lock is
    /// released.
    pub fn signal_msi_for(&self, its: ItsId, device: ItsDeviceId, event: EventId) -> VgicResult {
        let backing = {
            let state = self.native.inner.state.lock_irqsave();
            state.msi_backings.get(&(its, device, event)).copied()
        };
        match backing {
            Some(MsiBacking::Physical(binding)) => {
                return backend_result(self.native.inner.backend.signal_physical_msi(binding));
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
        let (lpi, target) = {
            let states = self.its.states().lock();
            states
                .get(&its)
                .ok_or_else(|| VgicError::ResourceNotFound {
                    resource: alloc::format!("ITS {}", its.value()),
                    operation: "signal MSI",
                })?
                .translate(device, event)?
        };
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
        // This path must not allocate: an MSI can be signalled from a hard IRQ.
        // The target record is materialized in task context when the guest
        // programs the ITS translation (`MAPTI`/`MOVI` emit
        // `ItsAction::Prepare`), so `set_lpi_pending` only looks up existing
        // records and reports `Unprepared` rather than growing storage here.
        // The raw guard owns only the short pending update: a detached target
        // is reported after the guard, so no error message is formatted while
        // the canonical state is locked.
        let delivery = {
            let mut state = self.native.inner.state.lock_irqsave();
            state.set_lpi_pending(target, lpi, true)
        };
        let wake = delivery.map_err(state::LpiDeliveryFailure::into_vgic_error)?;
        wake_vcpu(wake)
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
            Some(SpiBacking::Physical(_)) => Err(VgicError::NativeState {
                operation,
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "the SPI is electrically driven by its physical backing",
                kind: crate::StateErrorKind::Unsupported,
                detail: crate::NativeStateDetail::None,
            }),
            None if config.spi_ownership() == GicV3SpiOwnership::AllGuestOwned => Ok(()),
            None => Err(VgicError::NativeState {
                operation,
                vcpu: None,
                intid: Some(IntId::Spi(spi)),
                reason: "the SPI is not owned by this VM",
                kind: crate::StateErrorKind::InvalidState,
                detail: crate::NativeStateDetail::None,
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

impl GicV3Native {
    /// Publishes a wired interrupt into canonical native state.
    pub fn inject(
        &self,
        vcpu: usize,
        intid: u32,
        trigger: axvm_types::InterruptTriggerMode,
    ) -> VgicResult {
        match IntId::new(intid)? {
            IntId::Sgi(sgi) => self.send_sgi(GicVcpuId::new(vcpu), sgi, crate::SgiTarget::SelfOnly),
            IntId::Ppi(ppi) => match trigger {
                axvm_types::InterruptTriggerMode::EdgeTriggered => {
                    self.pulse_ppi(GicVcpuId::new(vcpu), ppi)
                }
                axvm_types::InterruptTriggerMode::LevelTriggered => {
                    self.set_ppi_level(GicVcpuId::new(vcpu), ppi, true)
                }
            },
            IntId::Spi(spi) => match trigger {
                axvm_types::InterruptTriggerMode::EdgeTriggered => self.pulse_spi(spi),
                axvm_types::InterruptTriggerMode::LevelTriggered => self.set_spi_level(spi, true),
            },
            IntId::Lpi(lpi) => Err(crate::VgicError::NativeState {
                operation: "inject wired interrupt",
                vcpu: Some(vcpu),
                intid: Some(IntId::Lpi(lpi)),
                reason: "LPIs must be delivered through an ITS endpoint",
                kind: crate::StateErrorKind::Unsupported,
                detail: crate::NativeStateDetail::None,
            }),
        }
    }
}
