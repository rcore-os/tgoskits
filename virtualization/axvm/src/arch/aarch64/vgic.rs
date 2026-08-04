//! AArch64 VM-local VGIC construction and activation lifecycle.

use alloc::{sync::Arc, vec::Vec};

use arm_vgic::{
    ArmVgicConfig, AssignedSpiConfig, GicAffinity, GicV3Backend, GicV3VcpuBinding, GicV3VcpuWake,
    GicVcpuId, HostGicVersion, PpiId, SpiId, TriggerMode, VgicAccessContext, VgicCore,
    VgicDeviceSet, VgicError, VgicMmioRegion, VgicResult, VgicV2Config, VgicV3Config,
};
use ax_kspin::SpinNoIrq;
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceManagerError,
    DeviceManagerResult, DeviceRegistration, ServiceCardinality, ServiceKey,
    VirtualInterruptControllerKey, validate_device_config,
};
use axdevice_base::{HostIrqId, VirtualInterruptController};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use super::{
    gic::{self, AssignedSpiRoutes},
    vtimer,
};
use crate::{AxVmError, AxVmResult, irq::deferred::DeferredVcpuKick, machine::GuestTimerProfile};

const REDISTRIBUTOR_STRIDE: u64 = 0x2_0000;

/// vCPU-local VGIC resources derived from the machine timer profile.
pub(crate) struct Aarch64VcpuIrqBinding {
    pub(crate) gic: GicV3VcpuBinding,
    pub(crate) backend: Arc<gic::AxvmVgicBackend>,
    pub(crate) virtual_timer_ppi: PpiId,
    pub(crate) physical_timer_ppi: PpiId,
    pub(crate) host_virtual_timer_intid: u32,
}

/// Typed VM-local service for vCPU attachment and physical-source lifecycle.
pub(crate) struct Aarch64VgicRuntimeKey;

impl ServiceKey for Aarch64VgicRuntimeKey {
    type Service = Aarch64VgicRuntime;

    const NAME: &'static str = "aarch64-vgic-runtime";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

enum RuntimePhase {
    Inactive,
    Activating,
    Active(Arc<AssignedSpiRoutes>),
    Deactivating,
}

/// VM-owned control-plane state that is deliberately separate from IRQ state.
pub(crate) struct Aarch64VgicRuntime {
    core: Arc<VgicCore>,
    backend: Arc<gic::AxvmVgicBackend>,
    kick: Arc<DeferredVcpuKick>,
    host_virtual_timer_intid: u32,
    phase: SpinNoIrq<RuntimePhase>,
}

impl Aarch64VgicRuntime {
    fn new(
        vm_id: usize,
        core: Arc<VgicCore>,
        backend: Arc<gic::AxvmVgicBackend>,
        host_virtual_timer_intid: u32,
    ) -> Arc<Self> {
        Arc::new(Self {
            core,
            backend,
            kick: DeferredVcpuKick::new(vm_id),
            host_virtual_timer_intid,
            phase: SpinNoIrq::new(RuntimePhase::Inactive),
        })
    }

    pub(crate) fn core(&self) -> &Arc<VgicCore> {
        &self.core
    }

    pub(crate) fn attach_vcpu(
        &self,
        vcpu_id: usize,
        timer_profile: &GuestTimerProfile,
    ) -> VgicResult<Aarch64VcpuIrqBinding> {
        let gic = self.core.attach_vcpu(
            vcpu_id,
            Arc::new(Aarch64VcpuWake {
                kick: self.kick.clone(),
                vcpu_id,
            }),
        )?;
        let virtual_timer_ppi = timer_ppi(timer_profile.virtual_intid)?;
        let physical_timer_ppi = timer_ppi(timer_profile.nonsecure_physical_intid)?;
        for ppi in [virtual_timer_ppi, physical_timer_ppi] {
            self.core.controller().configure_ppi_input(
                GicVcpuId::new(vcpu_id),
                ppi,
                TriggerMode::Level,
            )?;
        }
        Ok(Aarch64VcpuIrqBinding {
            gic,
            backend: self.backend.clone(),
            virtual_timer_ppi,
            physical_timer_ppi,
            host_virtual_timer_intid: timer_profile.virtual_intid,
        })
    }

    /// Claims host sources and publishes their fixed hard-IRQ routes.
    pub(crate) fn activate(&self) -> AxVmResult {
        {
            let mut phase = self.phase.lock();
            match &*phase {
                RuntimePhase::Inactive => *phase = RuntimePhase::Activating,
                RuntimePhase::Active(_) => return Ok(()),
                RuntimePhase::Activating | RuntimePhase::Deactivating => {
                    return Err(AxVmError::resource_conflict(
                        "AArch64 VGIC lifecycle",
                        "another lifecycle transition is in progress",
                    ));
                }
            }
        }

        if let Err(error) = vtimer::ensure_host_timer_ppi(self.host_virtual_timer_intid) {
            *self.phase.lock() = RuntimePhase::Inactive;
            return Err(error);
        }

        self.kick.start();
        if let Err(error) = self.core.bind_assigned_spis() {
            self.kick.stop();
            *self.phase.lock() = RuntimePhase::Inactive;
            return Err(AxVmError::interrupt("bind assigned physical SPIs", error));
        }

        let routes = match gic::register_assigned_spi_routes(&self.core) {
            Ok(routes) => routes,
            Err(error) => {
                if let Err(rollback_error) = self.core.unbind_assigned_spis() {
                    warn!(
                        "failed to roll back AArch64 assigned SPIs after route error: \
                         {rollback_error}"
                    );
                }
                self.kick.stop();
                *self.phase.lock() = RuntimePhase::Inactive;
                return Err(AxVmError::interrupt(
                    "register assigned physical SPI routes",
                    error,
                ));
            }
        };
        *self.phase.lock() = RuntimePhase::Active(routes);
        Ok(())
    }

    /// Removes routes only after every physical delivery is quiescent.
    pub(crate) fn deactivate(&self) -> AxVmResult {
        let routes = {
            let mut phase = self.phase.lock();
            match core::mem::replace(&mut *phase, RuntimePhase::Deactivating) {
                RuntimePhase::Inactive => {
                    *phase = RuntimePhase::Inactive;
                    return Ok(());
                }
                RuntimePhase::Active(routes) => routes,
                RuntimePhase::Activating | RuntimePhase::Deactivating => {
                    *phase = RuntimePhase::Deactivating;
                    return Err(AxVmError::resource_conflict(
                        "AArch64 VGIC lifecycle",
                        "another lifecycle transition is in progress",
                    ));
                }
            }
        };

        routes.quiesce();
        if let Err(error) = self.core.teardown_assigned_spis() {
            routes.resume();
            *self.phase.lock() = RuntimePhase::Active(routes);
            return Err(AxVmError::interrupt(
                "tear down assigned physical SPIs",
                error,
            ));
        }

        // Dropping the route handles removes the static hard-IRQ lookup before
        // the task-context kick worker is stopped.
        drop(routes);
        self.kick.stop();
        *self.phase.lock() = RuntimePhase::Inactive;
        Ok(())
    }
}

fn timer_ppi(intid: u32) -> VgicResult<PpiId> {
    let raw = u8::try_from(intid).map_err(|_| VgicError::InvalidIntId { raw: intid })?;
    PpiId::new(raw)
}

impl Drop for Aarch64VgicRuntime {
    fn drop(&mut self) {
        if let Err(error) = self.deactivate() {
            warn!("failed to deactivate AArch64 VGIC runtime while dropping it: {error:?}");
        }
    }
}

struct Aarch64VcpuWake {
    kick: Arc<DeferredVcpuKick>,
    vcpu_id: usize,
}

impl GicV3VcpuWake for Aarch64VcpuWake {
    fn wake(&self) -> VgicResult {
        self.kick
            .publish_from_irq(self.vcpu_id)
            .map_err(|error| VgicError::Backend {
                operation: "publish deferred AArch64 vCPU kick",
                detail: alloc::format!("{error}"),
            })
    }
}

struct Aarch64VgicFactory {
    vm_id: usize,
    expected: EmulatedDeviceConfig,
    runtime: Arc<Aarch64VgicRuntime>,
}

impl DeviceFactory for Aarch64VgicFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        validate_device_config(&self.expected, config, "build AArch64 virtual GIC")?;
        let access_context: Arc<dyn VgicAccessContext> =
            Arc::new(AxvmVgicAccessContext { vm_id: self.vm_id });
        let devices = VgicDeviceSet::new(self.runtime.core.clone(), access_context)
            .map_err(|error| vgic_device_error("build AArch64 virtual GIC frontends", error))?;
        let mut bundle = DeviceBundle::new();
        for device in devices.into_devices() {
            bundle.push(DeviceRegistration::Device(device));
        }
        let controller: Arc<dyn VirtualInterruptController> = self.runtime.core.clone();
        bundle
            .with_service::<Aarch64VgicRuntimeKey>(self.runtime.clone())?
            .with_service::<VirtualInterruptControllerKey>(controller)
    }
}

struct Aarch64GicCpuRegionMarkerFactory {
    expected: EmulatedDeviceConfig,
}

impl DeviceFactory for Aarch64GicCpuRegionMarkerFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::GicCpuRegion
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        validate_device_config(
            &self.expected,
            config,
            "validate AArch64 virtual GIC per-CPU region",
        )?;
        // The Distributor contribution atomically registers every frontend
        // from one VgicCore. This marker consumes the machine descriptor so
        // neither the GICC nor Redistributor window gets a second construction
        // path.
        Ok(DeviceBundle::new())
    }
}

struct AxvmVgicAccessContext {
    vm_id: usize,
}

impl VgicAccessContext for AxvmVgicAccessContext {
    fn current_vcpu(&self) -> Option<usize> {
        (crate::current_vm_id() == Some(self.vm_id))
            .then(crate::current_vcpu_id)
            .flatten()
    }
}

/// Creates the canonical controller and registers its only construction path.
pub(crate) fn register_device_factories(
    vm: &crate::vm::AxVM,
    registry: &mut DeviceFactoryRegistry,
) -> AxVmResult<Arc<Aarch64VgicRuntime>> {
    let (configs, placements, passthrough_irqs, gic_profile, timer_profile) =
        vm.with_config(|config| {
            (
                config.emu_devices().clone(),
                config.phys_cpu_ls.get_vcpu_affinities_pcpu_ids(),
                config.pass_through_irqs().to_vec(),
                config.gic_profile().cloned(),
                config.timer_profile().cloned(),
            )
        });
    let distributor = unique_config(
        &configs,
        EmulatedDeviceType::InterruptController,
        "AArch64 virtual GIC Distributor",
    )?;
    let cpu_region_descriptor = unique_config(
        &configs,
        EmulatedDeviceType::GicCpuRegion,
        "AArch64 virtual GIC per-CPU region",
    )?;

    let backend =
        gic::backend().map_err(|error| AxVmError::interrupt("create host GIC backend", error))?;
    let capabilities = backend.capabilities();
    let guest_version = match gic_profile.as_ref().map(|profile| profile.cpu_region) {
        Some(crate::machine::GuestGicCpuRegion::CpuInterface(_)) => HostGicVersion::V2,
        Some(crate::machine::GuestGicCpuRegion::Redistributors(_)) | None => HostGicVersion::V3,
    };
    if capabilities.host_version() != guest_version {
        return Err(AxVmError::unsupported(
            "create AArch64 virtual GIC",
            alloc::format!(
                "machine profile requires {guest_version:?}, but the host CPU interface is {:?}",
                capabilities.host_version()
            ),
        ));
    }
    let affinities = placements
        .iter()
        .map(|(_, _, physical_id)| GicAffinity::from_mpidr(*physical_id as u64))
        .collect();
    let assigned_spis = assigned_spis(&passthrough_irqs)?;
    let controller_id = axdevice_base::InterruptControllerId::new(0);
    let distributor_region =
        VgicMmioRegion::new(distributor.base_gpa as u64, distributor.length as u64)
            .map_err(|error| AxVmError::interrupt("validate GIC Distributor range", error))?;
    let cpu_region = VgicMmioRegion::new(
        cpu_region_descriptor.base_gpa as u64,
        cpu_region_descriptor.length as u64,
    )
    .map_err(|error| AxVmError::interrupt("validate GIC per-CPU range", error))?;
    let spi_count = gic::host_spi_count()
        .map_err(|error| AxVmError::interrupt("inspect host GIC SPI capacity", error))?;
    let vgic_config = match guest_version {
        HostGicVersion::V2 => {
            if !cpu_region_descriptor.cfg_list.is_empty() {
                return Err(AxVmError::invalid_config(
                    "AArch64 GICv2 CPU-interface descriptor must not carry a vCPU count",
                ));
            }
            ArmVgicConfig::V2(
                VgicV2Config::new(controller_id, distributor_region, cpu_region, affinities)
                    .and_then(|config| config.with_spi_count(spi_count))
                    .and_then(|config| {
                        config.with_list_register_count(capabilities.list_register_count())
                    })
                    .and_then(|config| config.with_priority_bits(capabilities.priority_bits()))
                    .and_then(|config| config.with_assigned_spis(assigned_spis))
                    .map_err(|error| {
                        AxVmError::interrupt("construct AArch64 virtual GICv2", error)
                    })?,
            )
        }
        HostGicVersion::V3 => {
            let [configured_vcpu_count] = cpu_region_descriptor.cfg_list.as_slice() else {
                return Err(AxVmError::invalid_config(
                    "AArch64 redistributor descriptor requires one vCPU count",
                ));
            };
            if *configured_vcpu_count != placements.len() {
                return Err(AxVmError::invalid_config(alloc::format!(
                    "AArch64 redistributor descriptor names {} vCPUs, but placement has {}",
                    configured_vcpu_count,
                    placements.len()
                )));
            }
            ArmVgicConfig::V3(
                VgicV3Config::new(
                    controller_id,
                    distributor_region,
                    alloc::vec![cpu_region],
                    REDISTRIBUTOR_STRIDE,
                    affinities,
                )
                .and_then(|config| config.with_spi_count(spi_count))
                .and_then(|config| {
                    config.with_list_register_count(capabilities.list_register_count())
                })
                .and_then(|config| config.with_priority_bits(capabilities.priority_bits()))
                .and_then(|config| config.with_assigned_spis(assigned_spis))
                .map_err(|error| AxVmError::interrupt("construct AArch64 virtual GICv3", error))?,
            )
        }
    };
    let core = Arc::new(
        VgicCore::new(vgic_config, backend.clone())
            .map_err(|error| AxVmError::interrupt("create AArch64 virtual GIC", error))?,
    );
    let host_virtual_timer_intid = timer_profile
        .ok_or_else(|| {
            AxVmError::invalid_config("AArch64 machine profile has no architectural timer")
        })?
        .virtual_intid;
    let runtime = Aarch64VgicRuntime::new(vm.id(), core, backend, host_virtual_timer_intid);

    registry.register(Arc::new(Aarch64VgicFactory {
        vm_id: vm.id(),
        expected: distributor.clone(),
        runtime: runtime.clone(),
    }))?;
    registry.register(Arc::new(Aarch64GicCpuRegionMarkerFactory {
        expected: cpu_region_descriptor.clone(),
    }))?;
    Ok(runtime)
}

fn assigned_spis(
    configured: &[crate::config::PassthroughInterrupt],
) -> AxVmResult<Vec<AssignedSpiConfig>> {
    configured
        .iter()
        .map(|route| {
            let intid = route.source.checked_add(32).ok_or_else(|| {
                AxVmError::invalid_config("AArch64 passthrough SPI number overflows")
            })?;
            AssignedSpiConfig::new(
                SpiId::new(intid)
                    .map_err(|error| AxVmError::interrupt("validate assigned SPI", error))?,
                HostIrqId::new(intid as usize),
                0,
                route.trigger,
            )
            .map_err(|error| AxVmError::interrupt("plan assigned physical SPI", error))
        })
        .collect()
}

fn unique_config<'a>(
    configs: &'a [EmulatedDeviceConfig],
    device_type: EmulatedDeviceType,
    resource: &'static str,
) -> AxVmResult<&'a EmulatedDeviceConfig> {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == device_type);
    let config = matches
        .next()
        .ok_or_else(|| AxVmError::resource_unavailable("machine device", resource))?;
    if matches.next().is_some() {
        return Err(AxVmError::resource_conflict(
            "machine device",
            alloc::format!("more than one {resource} descriptor is configured"),
        ));
    }
    Ok(config)
}

fn vgic_device_error(operation: &'static str, error: VgicError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation,
        detail: alloc::format!("{error}"),
    }
}
