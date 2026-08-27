//! AArch64 VM-local VGIC construction and activation lifecycle.

mod guest_memory;
mod plan;

use std::sync::Arc;

use arm_vgic::*;
use ax_std::os::arceos::sync::IrqSafeMutex;
use axdevice::*;
use axdevice_base::{MessageInterruptController, VirtualInterruptController};
pub(super) use plan::VgicConstructionPlan;

use super::{
    gic::{self, AssignedSpiRoutes},
    vtimer,
};
use crate::{irq::deferred::*, machine::*, *};

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
    phase: IrqSafeMutex<RuntimePhase>,
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
            phase: IrqSafeMutex::new(RuntimePhase::Inactive),
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
            match std::mem::replace(&mut *phase, RuntimePhase::Deactivating) {
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
                detail: std::format!("{error}"),
            })
    }
}

struct Aarch64VgicFactory {
    vm_id: usize,
    plan: Arc<VgicConstructionPlan>,
}

impl DeviceModel for Aarch64VgicFactory {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        self.plan.requirements()
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let mut fdt = match self.plan.config() {
            ArmVgicConfig::V2(_) => {
                FdtNodeSpec::new("interrupt-controller").with_compatible("arm,gic-400")
            }
            ArmVgicConfig::V3(_) => {
                FdtNodeSpec::new("interrupt-controller").with_compatible("arm,gic-v3")
            }
        }
        .with_register(ResourceSlot::new("distributor").expect("static VGIC slot is valid"));
        let mut acpi = AcpiDeviceSpec::table("GIC0")
            .with_register(ResourceSlot::new("distributor").expect("static VGIC slot is valid"));
        match self.plan.config() {
            ArmVgicConfig::V2(_) => {
                let slot = ResourceSlot::new("cpu-region-0").expect("static VGIC slot is valid");
                fdt = fdt.with_register(slot.clone());
                acpi = acpi.with_register(slot);
            }
            ArmVgicConfig::V3(config) => {
                for index in 0..config.redistributors().len() {
                    let slot = ResourceSlot::new(std::format!("cpu-region-{index}"))
                        .expect("generated VGIC slot is valid");
                    fdt = fdt.with_register(slot.clone());
                    acpi = acpi.with_register(slot);
                }
                for its in config.its() {
                    let slot = ResourceSlot::new(std::format!("its-{}", its.id().value()))
                        .expect("generated ITS slot is valid");
                    fdt = fdt.with_register(slot.clone());
                    acpi = acpi.with_register(slot);
                }
            }
        }
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::InterruptController {
                controller: self.plan.config().controller_id(),
                node: fdt,
            }]),
            Some(std::vec![AcpiContributionSpec::InterruptController {
                controller: self.plan.config().controller_id(),
                device: acpi,
            }]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        self.plan.validate_and_consume(context)?;
        let runtime = create_runtime(self.vm_id, &self.plan).map_err(|error| {
            DeviceManagerError::InvalidConfig {
                operation: "create AArch64 virtual GIC runtime",
                detail: std::format!("{error}"),
            }
        })?;
        let devices = VgicDeviceSet::new(runtime.core.clone())
            .map_err(|error| vgic_device_error("build AArch64 virtual GIC frontends", error))?;
        let mut bundle = DeviceBundle::new();
        for device in devices.into_devices() {
            bundle.push(DeviceRegistration::Device(device));
        }
        let controller: Arc<dyn VirtualInterruptController> = runtime.core.clone();
        let mut registration = ControllerRegistration::new(runtime.core.id(), controller);
        if matches!(
            runtime.core.config(),
            ArmVgicConfig::V3(config) if !config.its().is_empty()
        ) {
            let message: Arc<dyn MessageInterruptController> = runtime.core.clone();
            registration = registration.with_message(message);
        }
        bundle.push(DeviceRegistration::InterruptController(registration));
        bundle.with_service::<Aarch64VgicRuntimeKey>(runtime)
    }
}

/// Creates the canonical controller model used by the AArch64 device graph.
pub(crate) fn model(vm_id: usize, plan: &Arc<VgicConstructionPlan>) -> Arc<dyn DeviceModel> {
    Arc::new(Aarch64VgicFactory {
        vm_id,
        plan: plan.clone(),
    })
}

fn create_runtime(
    vm_id: usize,
    plan: &Arc<VgicConstructionPlan>,
) -> AxVmResult<Arc<Aarch64VgicRuntime>> {
    let backend = plan.backend();
    let guest_memory = matches!(
        plan.config(),
        ArmVgicConfig::V3(config) if !config.its().is_empty()
    )
    .then(|| Arc::new(guest_memory::AxvmGuestMemory::new(vm_id)) as Arc<dyn arm_vgic::GuestMemory>);
    let core = Arc::new(
        VgicCore::new_with_guest_memory(plan.config().clone(), backend.clone(), guest_memory)
            .map_err(|error| AxVmError::interrupt("create AArch64 virtual GIC", error))?,
    );
    let runtime = Aarch64VgicRuntime::new(vm_id, core, backend, plan.host_virtual_timer_intid());

    Ok(runtime)
}

fn vgic_device_error(operation: &'static str, error: VgicError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation,
        detail: std::format!("{error}"),
    }
}
