//! AArch64 VM-local VGIC construction and activation lifecycle.

mod guest_memory;
mod plan;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use arm_vgic::*;
use ax_std::os::arceos::sync::RawSpinLock;
use axdevice::*;
use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptEndpoint, InterruptTriggerMode, IrqError,
    IrqResult, MessageInterruptController, VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
pub(super) use plan::VgicConstructionPlan;

use super::{
    gic::{self, AssignedSpiRoutes},
    vtimer,
};
use crate::{
    AxVmResult, RunId,
    guest_memory::GuestMemoryPort,
    irq::model::{
        InterruptControllerEndpoint, InterruptControllerOwner, InterruptSourceId, SourceEvent,
    },
    machine::*,
    services::RunSignals,
    sync::MutexExt,
    *,
};

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
    vm_id: VMId,
    core: Arc<VgicCore>,
    /// Native delivery/CPU-interface port. Hardware-facing holders (the run
    /// entry, vCPU backends, and the fixed host-IRQ route slots) keep this
    /// instead of the full `VgicCore`, so they never transitively retain the
    /// sleepable software-ITS state.
    native: GicV3Native,
    backend: Arc<gic::AxvmVgicBackend>,
    /// Task-only software-ITS guest-memory adapter. It stays out of the hardware
    /// entry so the sleepable guest-memory capability is reachable only from the
    /// run's task-context GITS write path.
    its_memory: Option<Arc<guest_memory::AxvmGuestMemory>>,
    host_virtual_timer_intid: u32,
    /// Lifecycle transitions run on the control owner and may take sleepable
    /// host locks, so the transition phase uses an ordinary task-context mutex.
    phase: Mutex<RuntimePhase>,
    /// Run-bound kick target published before any vCPU can run. It is read from
    /// the VGIC host-IRQ wake path, so the value uses a raw lock and the guard
    /// is released before the deferred kick is published. The wake callback
    /// shares only this raw slot, never the full runtime.
    run: Arc<RawSpinLock<Option<Arc<RunSignals>>>>,
    inputs: Mutex<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
    wired_sink: Arc<Aarch64VgicWiredSink>,
}

impl Aarch64VgicRuntime {
    fn new(
        vm_id: usize,
        core: Arc<VgicCore>,
        backend: Arc<gic::AxvmVgicBackend>,
        its_memory: Option<Arc<guest_memory::AxvmGuestMemory>>,
        host_virtual_timer_intid: u32,
    ) -> Arc<Self> {
        let native = core.controller().native_port();
        Arc::new_cyclic(|runtime| Self {
            vm_id,
            core,
            native,
            backend,
            its_memory,
            host_virtual_timer_intid,
            phase: Mutex::new(RuntimePhase::Inactive),
            run: Arc::new(RawSpinLock::new(None)),
            inputs: Mutex::new(BTreeMap::new()),
            wired_sink: Arc::new(Aarch64VgicWiredSink {
                runtime: runtime.clone(),
            }),
        })
    }

    pub(crate) fn native(&self) -> &GicV3Native {
        &self.native
    }

    /// Binds the run's guest-memory capability into the task-only ITS adapter.
    ///
    /// Task-context only and exactly once per run. A VM without an ITS has no
    /// adapter, so the capability is simply unused.
    pub(crate) fn bind_task_memory(&self, memory: GuestMemoryPort) -> AxVmResult {
        match &self.its_memory {
            Some(adapter) => adapter.bind(memory),
            None => Ok(()),
        }
    }

    /// Seals this runtime to exactly one execution period.
    ///
    /// The binding records the run identity, so a stale port or a retired run
    /// can never re-target the controller of a newer run.
    pub(crate) fn bind_run(&self, signals: &Arc<RunSignals>) -> AxVmResult {
        let conflict = {
            let mut bound = self.run.lock_irqsave();
            match bound.as_ref() {
                Some(existing) if existing.run_id() == signals.run_id() => false,
                Some(_) => true,
                None => {
                    *bound = Some(signals.clone());
                    false
                }
            }
        };
        if conflict {
            // The raw guard is released before the error allocates a message.
            return Err(AxVmError::resource_conflict(
                "bind AArch64 VGIC run",
                "the runtime is already bound to another execution period",
            ));
        }
        Ok(())
    }

    /// Clears the binding only when `run` still owns it.
    pub(crate) fn unbind_run(&self, run: RunId) -> AxVmResult {
        // The retired target is dropped after the raw guard is released.
        let removed = {
            let mut bound = self.run.lock_irqsave();
            if bound
                .as_ref()
                .is_some_and(|signals| signals.run_id() == run)
            {
                bound.take()
            } else {
                None
            }
        };
        drop(removed);
        Ok(())
    }

    pub(crate) fn attach_vcpu(
        &self,
        vcpu_id: usize,
        timer_profile: &GuestTimerProfile,
    ) -> VgicResult<Aarch64VcpuIrqBinding> {
        let gic = self.core.attach_vcpu(
            vcpu_id,
            Arc::new(Aarch64VcpuWake {
                run: Arc::clone(&self.run),
                vm_id: self.vm_id,
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
            let mut phase = self.phase.lock_unpoisoned();
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
            *self.phase.lock_unpoisoned() = RuntimePhase::Inactive;
            return Err(error);
        }

        if let Err(error) = self.core.bind_assigned_spis() {
            let primary = AxVmError::interrupt("bind assigned physical SPIs", error);
            *self.phase.lock_unpoisoned() = RuntimePhase::Inactive;
            return Err(primary);
        }

        let routes = match gic::register_assigned_spi_routes(
            &self.native,
            self.core.config().assigned_spis(),
        ) {
            Ok(routes) => routes,
            Err(error) => {
                let primary = AxVmError::interrupt("register assigned physical SPI routes", error);
                let unbind = self.core.unbind_assigned_spis();
                *self.phase.lock_unpoisoned() = RuntimePhase::Inactive;
                return match unbind {
                    Ok(()) => Err(primary),
                    Err(rollback) => Err(AxVmError::lifecycle_rollback(
                        "activate AArch64 VGIC runtime",
                        primary,
                        rollback,
                    )),
                };
            }
        };
        *self.phase.lock_unpoisoned() = RuntimePhase::Active(routes);
        Ok(())
    }

    /// Removes routes only after every physical delivery is quiescent.
    pub(crate) fn deactivate(&self) -> AxVmResult {
        let routes = {
            let mut phase = self.phase.lock_unpoisoned();
            match std::mem::replace(&mut *phase, RuntimePhase::Deactivating) {
                RuntimePhase::Inactive => {
                    *phase = RuntimePhase::Inactive;
                    return Ok(());
                }
                RuntimePhase::Active(routes) => routes,
                transition @ (RuntimePhase::Activating | RuntimePhase::Deactivating) => {
                    *phase = transition;
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
            *self.phase.lock_unpoisoned() = RuntimePhase::Active(routes);
            return Err(AxVmError::interrupt(
                "tear down assigned physical SPIs",
                error,
            ));
        }

        // Dropping the route handles removes the static hard-IRQ lookup before
        // the run binding is released by the control owner.
        drop(routes);
        *self.phase.lock_unpoisoned() = RuntimePhase::Inactive;
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

impl InterruptControllerEndpoint for Aarch64VgicRuntime {
    type Error = AxVmError;

    fn id(&self) -> InterruptControllerId {
        <VgicCore as VirtualInterruptController>::id(self.core.as_ref())
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        let spi = u32::try_from(input.value())
            .ok()
            .and_then(|value| SpiId::new(value).ok())
            .ok_or_else(|| IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: <Self as InterruptControllerEndpoint>::id(self),
                    input,
                },
                operation: "open AArch64 VGIC input",
                detail: "the input is outside the VGIC SPI range".into(),
            })?;

        if let Some((actual, existing)) = self.inputs.lock_unpoisoned().get(&input.value()) {
            if *actual != trigger {
                return Err(IrqError::InvalidTriggerMode {
                    endpoint: InterruptEndpoint::Wired {
                        controller: <Self as InterruptControllerEndpoint>::id(self),
                        input,
                    },
                    operation: "open AArch64 VGIC input",
                    expected: *actual,
                    actual: trigger,
                });
            }
            return Ok(existing.clone());
        }

        let trigger_mode = match trigger {
            InterruptTriggerMode::EdgeTriggered => TriggerMode::Edge,
            InterruptTriggerMode::LevelTriggered => TriggerMode::Level,
        };
        self.core
            .controller()
            .configure_spi_input(spi, trigger_mode)
            .map_err(|error| IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: <Self as InterruptControllerEndpoint>::id(self),
                    input,
                },
                operation: "configure AArch64 VGIC input",
                detail: error.to_string(),
            })?;

        let wired = WiredIrqInput::new(
            <Self as InterruptControllerEndpoint>::id(self),
            input,
            trigger,
            self.wired_sink.clone(),
        );
        let mut inputs = self.inputs.lock_unpoisoned();
        if let Some((actual, existing)) = inputs.get(&input.value()) {
            if *actual != trigger {
                return Err(IrqError::InvalidTriggerMode {
                    endpoint: InterruptEndpoint::Wired {
                        controller: <Self as InterruptControllerEndpoint>::id(self),
                        input,
                    },
                    operation: "open AArch64 VGIC input",
                    expected: *actual,
                    actual: trigger,
                });
            }
            return Ok(existing.clone());
        }
        inputs.insert(input.value(), (trigger, wired.clone()));
        Ok(wired)
    }

    fn submit(&self, event: SourceEvent) -> AxVmResult {
        let epoch = match event {
            SourceEvent::Pulse { epoch, .. }
            | SourceEvent::Level { epoch, .. }
            | SourceEvent::Eoi { epoch, .. } => epoch,
        };
        let signals = self
            .run
            .lock_irqsave()
            .as_ref()
            .cloned()
            .ok_or_else(|| AxVmError::interrupt("submit AArch64 interrupt", "no active run"))?;
        if signals.epoch() != epoch {
            return Err(AxVmError::StaleRun {
                expected: epoch.run(),
                current: Some(signals.run_id()),
            });
        }
        signals
            .publish_controller_event(event)
            .map_err(|error| AxVmError::interrupt("queue AArch64 interrupt event", error))
    }
}

struct Aarch64VgicWiredSink {
    runtime: Weak<Aarch64VgicRuntime>,
}

impl Aarch64VgicWiredSink {
    fn endpoint(runtime: &Aarch64VgicRuntime, input: ControllerInputId) -> InterruptEndpoint {
        InterruptEndpoint::Wired {
            controller: <Aarch64VgicRuntime as InterruptControllerEndpoint>::id(runtime),
            input,
        }
    }

    fn submit(
        &self,
        input: ControllerInputId,
        event: SourceEvent,
        operation: &'static str,
    ) -> IrqResult {
        let runtime = self.runtime.upgrade().ok_or_else(|| IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: InterruptControllerId::new(0),
                input,
            },
            operation,
            detail: "AArch64 VGIC runtime is closed".into(),
        })?;
        InterruptControllerEndpoint::submit(runtime.as_ref(), event).map_err(|error| {
            IrqError::Backend {
                endpoint: Self::endpoint(runtime.as_ref(), input),
                operation,
                detail: error.to_string(),
            }
        })
    }
}

impl WiredIrqSink for Aarch64VgicWiredSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let runtime = self.runtime.upgrade().ok_or_else(|| IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: InterruptControllerId::new(0),
                input,
            },
            operation: "submit AArch64 VGIC line level",
            detail: "AArch64 VGIC runtime is closed".into(),
        })?;
        let signals = runtime
            .run
            .lock_irqsave()
            .as_ref()
            .cloned()
            .ok_or_else(|| IrqError::Backend {
                endpoint: Self::endpoint(runtime.as_ref(), input),
                operation: "submit AArch64 VGIC line level",
                detail: "AArch64 VGIC has no active run".into(),
            })?;
        self.submit(
            input,
            SourceEvent::Level {
                epoch: signals.epoch(),
                source: InterruptSourceId::new(
                    <Aarch64VgicRuntime as InterruptControllerEndpoint>::id(runtime.as_ref()),
                    input.value() as u32,
                    None,
                ),
                asserted,
            },
            "submit AArch64 VGIC line level",
        )
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        let runtime = self.runtime.upgrade().ok_or_else(|| IrqError::Backend {
            endpoint: InterruptEndpoint::Wired {
                controller: InterruptControllerId::new(0),
                input,
            },
            operation: "submit AArch64 VGIC line pulse",
            detail: "AArch64 VGIC runtime is closed".into(),
        })?;
        let signals = runtime
            .run
            .lock_irqsave()
            .as_ref()
            .cloned()
            .ok_or_else(|| IrqError::Backend {
                endpoint: Self::endpoint(runtime.as_ref(), input),
                operation: "submit AArch64 VGIC line pulse",
                detail: "AArch64 VGIC has no active run".into(),
            })?;
        self.submit(
            input,
            SourceEvent::Pulse {
                epoch: signals.epoch(),
                source: InterruptSourceId::new(
                    <Aarch64VgicRuntime as InterruptControllerEndpoint>::id(runtime.as_ref()),
                    input.value() as u32,
                    None,
                ),
            },
            "submit AArch64 VGIC line pulse",
        )
    }
}

impl InterruptControllerOwner for Aarch64VgicRuntime {
    type Error = AxVmError;

    fn apply_source(&self, event: SourceEvent) -> AxVmResult {
        let epoch = match event {
            SourceEvent::Pulse { epoch, .. }
            | SourceEvent::Level { epoch, .. }
            | SourceEvent::Eoi { epoch, .. } => epoch,
        };
        let signals = self
            .run
            .lock_irqsave()
            .as_ref()
            .cloned()
            .ok_or_else(|| AxVmError::interrupt("submit AArch64 interrupt", "no active run"))?;
        if signals.epoch() != epoch {
            return Err(AxVmError::StaleRun {
                expected: epoch.run(),
                current: Some(signals.run_id()),
            });
        }
        let source = match event {
            SourceEvent::Pulse { source, .. } | SourceEvent::Level { source, .. } => source,
            SourceEvent::Eoi { token, .. } => token.source,
        };
        if source.controller != <Self as InterruptControllerEndpoint>::id(self) {
            return Err(AxVmError::invalid_input(
                "submit AArch64 interrupt",
                "source belongs to another controller",
            ));
        }
        let spi = arm_vgic::SpiId::new(source.source)
            .map_err(|error| AxVmError::interrupt("validate AArch64 SPI source", error))?;
        match event {
            SourceEvent::Pulse { .. } => self
                .core
                .controller()
                .pulse_spi(spi)
                .map_err(|error| AxVmError::interrupt("pulse AArch64 SPI", error))?,
            SourceEvent::Level { asserted, .. } => self
                .core
                .controller()
                .set_spi_level(spi, asserted)
                .map_err(|error| AxVmError::interrupt("set AArch64 SPI level", error))?,
            SourceEvent::Eoi { token, .. } => {
                if token.target.run != epoch.run {
                    return Err(AxVmError::invalid_input(
                        "complete AArch64 interrupt",
                        "delivery token belongs to another run",
                    ));
                }
            }
        }
        Ok(())
    }
}

impl VirtualInterruptController for Aarch64VgicRuntime {
    fn id(&self) -> InterruptControllerId {
        <Self as InterruptControllerEndpoint>::id(self)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        <Self as InterruptControllerEndpoint>::wired_input(self, input, trigger)
    }
}

struct Aarch64VcpuWake {
    run: Arc<RawSpinLock<Option<Arc<RunSignals>>>>,
    vm_id: VMId,
    vcpu_id: usize,
}

impl GicV3VcpuWake for Aarch64VcpuWake {
    fn wake(&self) -> VgicResult {
        if crate::vcpu::with_current_execution(|current| {
            current.is_some_and(|execution| {
                execution.vm_id() == self.vm_id
                    && execution.vcpu_id() == self.vcpu_id
                    && execution.entry_loop_is_active()
            })
        }) {
            // Canonical VGIC state was published before this callback. The
            // owner loads it with local IRQs masked immediately before guest
            // entry; IRQs stay masked through guest exit and VGIC save. Thus
            // a local callback either precedes that load or follows the exit.
            // The scoped publication excludes migration and sleeping vCPUs.
            // Nonlocal and control-plane callbacks retain the deferred path.
            return Ok(());
        }
        publish_irq_kick(&self.run, self.vcpu_id)
    }
}

/// Publishes one deferred vCPU kick from a hard-IRQ or device callback.
///
/// The raw guard is released before the deferred worker is notified.
fn publish_irq_kick(run: &RawSpinLock<Option<Arc<RunSignals>>>, vcpu_id: usize) -> VgicResult {
    let signals = run.lock_irqsave().clone();
    let Some(signals) = signals else {
        return Err(VgicError::Backend {
            operation: "kick AArch64 vCPU from IRQ",
            source: arm_vgic::GicV3BackendError::new(
                "kick AArch64 vCPU from IRQ",
                "the VGIC runtime is not bound to a run",
            ),
        });
    };
    signals
        .kick_from_irq(vcpu_id)
        .map_err(|_| VgicError::Backend {
            operation: "kick AArch64 vCPU from IRQ",
            source: arm_vgic::GicV3BackendError::value(
                "kick AArch64 vCPU from IRQ",
                "the bound signal target rejected publication",
                vcpu_id as u64,
            ),
        })
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
        let controller: Arc<dyn VirtualInterruptController> = runtime.clone();
        let mut registration = ControllerRegistration::new(
            <Aarch64VgicRuntime as VirtualInterruptController>::id(&runtime),
            controller,
        );
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
    let its_memory = matches!(
        plan.config(),
        ArmVgicConfig::V3(config) if !config.its().is_empty()
    )
    .then(|| Arc::new(guest_memory::AxvmGuestMemory::new()));
    let guest_memory: Option<Arc<dyn arm_vgic::GuestMemory>> = its_memory
        .clone()
        .map(|memory| memory as Arc<dyn arm_vgic::GuestMemory>);
    let core = Arc::new(
        VgicCore::new_with_guest_memory(plan.config().clone(), backend.clone(), guest_memory)
            .map_err(|error| AxVmError::interrupt("create AArch64 virtual GIC", error))?,
    );
    let runtime = Aarch64VgicRuntime::new(
        vm_id,
        core,
        backend,
        its_memory,
        plan.host_virtual_timer_intid(),
    );

    Ok(runtime)
}

fn vgic_device_error(operation: &'static str, error: VgicError) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation,
        detail: std::format!("{error}"),
    }
}
