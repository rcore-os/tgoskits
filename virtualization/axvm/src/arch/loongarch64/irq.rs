//! LoongArch platform IRQ routing, run-bound interrupt ports and the
//! VM-local PCH-PIC/EIOINTC controller adapter.
//!
//! Publication into a run goes through a [`LoongArchRunPort`]: an immutable
//! capability that captures the exact [`RunId`] and the run's [`RunSignals`] at
//! run start. A callback that outlives its run therefore publishes into the
//! original (closed) run and is rejected by that run's own admission guard
//! instead of resolving a newer run by numeric VM id. Hard-IRQ sources use
//! fixed per-source slots that retain the same exact identity.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use ax_std::os::arceos::sync::RawSpinLock;
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFirmwareSpec, DeviceManagerError, DeviceManagerResult,
    DeviceModel, DeviceRequirements, LoongArchInterruptDomainFactory, LoongArchPchPic,
    LoongArchPchPicFactory, PchPicOutputEvent, PchPicOutputSink, ServiceCardinality, ServiceKey,
};
use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptEndpoint, IrqError, IrqResult,
    VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
use axvm_types::InterruptTriggerMode;

use crate::{
    AxVmResult, RunId, ax_err,
    irq::model::{PendingVcpuInterrupt, VirtualInterruptId},
    runtime::QueuedVcpuInterrupt,
    services::{RunSignals, SignalError},
    sync::MutexExt,
};

const PCH_PIC_INPUT_COUNT: usize = 64;
pub(crate) const LOONGARCH_MAX_IRQ_COUNT: usize = 256;
/// Every emulated PCH-PIC output is delivered to the boot vCPU, matching the
/// guest EIOINTC topology programmed by firmware.
const EXTERNAL_TARGET_VCPU: usize = 0;

/// Pre-bound capability to publish LoongArch interrupt sources into one run.
///
/// It carries the exact run identity and target; the run's closed/in-flight
/// admission guard is the only publication authority, so a stale capability can
/// never deliver into a later run.
#[derive(Clone)]
pub(crate) struct LoongArchRunPort {
    run: RunId,
    signals: Arc<RunSignals>,
}

impl LoongArchRunPort {
    pub(crate) fn new(signals: Arc<RunSignals>) -> Self {
        Self {
            run: signals.run_id(),
            signals,
        }
    }

    pub(crate) const fn run_id(&self) -> RunId {
        self.run
    }

    pub(crate) const fn vm_id(&self) -> usize {
        self.run.vm().vm_id()
    }

    /// Publishes one virtual source from hard-IRQ context.
    ///
    /// The queue publication precedes the IRQ-safe kick, which only hands the
    /// wake to the run's pre-bound deferred worker.
    pub(crate) fn publish_virtual_irq(
        &self,
        vcpu_id: usize,
        interrupt: PendingVcpuInterrupt,
    ) -> Result<(), SignalError> {
        self.signals.publish_queued(vcpu_id, interrupt.into())?;
        self.wake_recorded(vcpu_id)
    }

    /// Publishes one queued source from hard-IRQ context.
    pub(crate) fn publish_queued_irq(
        &self,
        vcpu_id: usize,
        interrupt: QueuedVcpuInterrupt,
    ) -> Result<(), SignalError> {
        self.signals.publish_queued(vcpu_id, interrupt)?;
        self.wake_recorded(vcpu_id)
    }

    /// Publishes one EIOINTC output vector produced by guest MMIO handling.
    pub(crate) fn publish_external(
        &self,
        vcpu_id: usize,
        vector: usize,
    ) -> Result<(), SignalError> {
        self.signals
            .publish_queued(vcpu_id, QueuedVcpuInterrupt::External { vector })?;
        self.wake_recorded(vcpu_id)
    }

    /// Completes a publication whose source is already in the run's fixed queue.
    ///
    /// [`RunSignals::publish_queued`] is the only step that records the source:
    /// when it rejects the publication nothing is pending and no later owner
    /// entry can observe it, so its error is reported to the caller instead of
    /// being presented as delivery. Once it returns, the queue owns the record;
    /// a target activation that retired before this wake leaves the source
    /// pending for its successor, so a skipped wake is not a delivery failure.
    fn wake_recorded(&self, vcpu_id: usize) -> Result<(), SignalError> {
        match self.signals.kick_from_irq(vcpu_id) {
            Ok(()) | Err(SignalError::InactiveTarget) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Builds the virtual source identity for one guest-internal vector.
    pub(crate) fn virtual_interrupt(vector: usize) -> crate::AxVmResult<PendingVcpuInterrupt> {
        let id = u32::try_from(vector).map_err(|_| {
            crate::AxVmError::invalid_input(
                "publish LoongArch interrupt",
                format!("interrupt vector {vector:#x} does not fit u32"),
            )
        })?;
        Ok(PendingVcpuInterrupt {
            id: VirtualInterruptId(id),
            trigger: InterruptTriggerMode::EdgeTriggered,
            source: None,
        })
    }
}

impl std::fmt::Debug for LoongArchRunPort {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoongArchRunPort")
            .field("run", &self.run)
            .finish()
    }
}

/// Lower per-run publication cell shared by the device and controller callbacks.
///
/// The owner binds the exact run port before guest entry is admitted and clears
/// it only after IRQ input is quiesced; a callback that observes no binding is
/// rejected instead of resolving a newer run.
pub(crate) struct LoongArchRunCell(RawSpinLock<Option<LoongArchRunPort>>);

impl LoongArchRunCell {
    const fn new() -> Self {
        Self(RawSpinLock::new(None))
    }

    fn bind(&self, port: LoongArchRunPort) {
        let previous = self.0.lock_irqsave().replace(port);
        drop(previous);
    }

    fn clear(&self) {
        let previous = self.0.lock_irqsave().take();
        drop(previous);
    }

    /// Clones the published port without holding the raw guard across a wake.
    fn current(&self) -> Option<LoongArchRunPort> {
        self.0.lock_irqsave().as_ref().cloned()
    }

    fn publish_external(&self, vcpu_id: usize, vector: usize) -> Result<(), SignalError> {
        match self.current() {
            Some(port) => port.publish_external(vcpu_id, vector),
            None => Err(SignalError::Closed),
        }
    }
}

impl std::fmt::Debug for LoongArchRunCell {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoongArchRunCell")
            .field("bound", &self.0.lock_irqsave().is_some())
            .finish()
    }
}

/// Per-run binding shared by the device model and its interrupt controller.
pub(crate) type LoongArchRunBinding = Arc<LoongArchRunCell>;

/// Creates the lower run cell owned by one VM plan.
pub(crate) fn new_run_binding() -> LoongArchRunBinding {
    Arc::new(LoongArchRunCell::new())
}

/// Typed access to the VM-local PCH-PIC runtime built for the current run.
pub(crate) struct LoongArchPchPicRuntimeKey;

impl ServiceKey for LoongArchPchPicRuntimeKey {
    type Service = LoongArchPchPicRuntime;

    const NAME: &'static str = "loongarch-pch-pic-runtime";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// VM-local PCH-PIC/EIOINTC adapter and its run-bound publication target.
pub(crate) struct LoongArchPchPicRuntime {
    pic: Arc<LoongArchPchPic>,
    run: LoongArchRunBinding,
    inputs: Mutex<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
}

impl LoongArchPchPicRuntime {
    fn new(pic: Arc<LoongArchPchPic>, run: LoongArchRunBinding) -> Arc<Self> {
        Arc::new(Self {
            pic,
            run,
            inputs: Mutex::new(BTreeMap::new()),
        })
    }

    /// Binds the exact run. Task context, before guest entry is admitted.
    pub(crate) fn activate(&self, port: LoongArchRunPort) {
        self.run.bind(port);
    }

    /// Retires the run binding after IRQ input has been quiesced.
    pub(crate) fn deactivate(&self) {
        self.run.clear();
    }
}

impl VirtualInterruptController for LoongArchPchPicRuntime {
    fn id(&self) -> InterruptControllerId {
        InterruptControllerId::new(0)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if input.value() >= PCH_PIC_INPUT_COUNT {
            return Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: self.id(),
                    input,
                },
                operation: "open LoongArch PCH-PIC input",
                detail: format!(
                    "input {} is outside 0..{PCH_PIC_INPUT_COUNT}",
                    input.value()
                ),
            });
        }
        // Registration and its fallible diagnostics are task-only, so they use a
        // sleeping mutex rather than a raw guard.
        let mut inputs = self.inputs.lock_unpoisoned();
        if let Some((registered_trigger, registered)) = inputs.get(&input.value()) {
            if *registered_trigger != trigger {
                return Err(IrqError::InvalidInput {
                    endpoint: InterruptEndpoint::Wired {
                        controller: self.id(),
                        input,
                    },
                    operation: "open LoongArch PCH-PIC input",
                    detail: format!(
                        "input {} is already registered as {registered_trigger:?}",
                        input.value()
                    ),
                });
            }
            return Ok(registered.clone());
        }

        let sink: Arc<dyn WiredIrqSink> = Arc::new(LoongArchPchPicIrqSink {
            pic: Arc::clone(&self.pic),
            run: Arc::clone(&self.run),
        });
        let registered = WiredIrqInput::new(self.id(), input, trigger, sink);
        inputs.insert(input.value(), (trigger, registered.clone()));
        Ok(registered)
    }
}

/// Wired-input sink that keeps the PCH-PIC as the sole owner of active state.
struct LoongArchPchPicIrqSink {
    pic: Arc<LoongArchPchPic>,
    run: LoongArchRunBinding,
}

impl WiredIrqSink for LoongArchPchPicIrqSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let vector = self.pic.set_irq_level(input.value(), asserted);
        if !asserted {
            return Ok(());
        }
        let Some(vector) = vector else {
            return Ok(());
        };
        // The controller state is already latched, but the queued EIOINTC
        // vector is the only record this guest can observe. A publication the
        // run rejects is therefore a real delivery failure, not a silent
        // success; only a skipped wake of an already-recorded source is benign.
        match self.run.publish_external(EXTERNAL_TARGET_VCPU, vector) {
            Ok(()) => Ok(()),
            Err(error) => Err(IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: InterruptControllerId::new(0),
                    input,
                },
                operation: "publish LoongArch PCH-PIC input",
                detail: error.to_string(),
            }),
        }
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.set_level(input, true)?;
        self.set_level(input, false)
    }
}

/// Sink the guest-visible PCH-PIC device uses for routing changes it produced.
pub(crate) struct LoongArchPchPicOutputSink {
    run: LoongArchRunBinding,
}

impl LoongArchPchPicOutputSink {
    pub(crate) fn new(run: LoongArchRunBinding) -> Self {
        Self { run }
    }
}

impl PchPicOutputSink for LoongArchPchPicOutputSink {
    fn publish(&self, event: PchPicOutputEvent) -> DeviceManagerResult {
        if !event.asserted {
            trace!(
                "LoongArch PCH-PIC deassert event for EIOINTC vector {}",
                event.vector
            );
            return Ok(());
        }
        match self
            .run
            .publish_external(EXTERNAL_TARGET_VCPU, event.vector)
        {
            // As above: a rejection means the guest never observes this vector,
            // so surface it; a recorded source whose wake was skipped is fine.
            Ok(()) => Ok(()),
            Err(error) => Err(DeviceManagerError::InvalidState {
                operation: "publish LoongArch PCH-PIC output",
                detail: error.to_string(),
            }),
        }
    }
}

/// Creates the VM-local runtime for each freshly built PCH-PIC instance.
struct LoongArchDomainFactory {
    run: LoongArchRunBinding,
    built: Mutex<Option<Arc<LoongArchPchPicRuntime>>>,
}

impl LoongArchInterruptDomainFactory for LoongArchDomainFactory {
    fn create(&self, pic: Arc<LoongArchPchPic>) -> Arc<dyn VirtualInterruptController> {
        let runtime = LoongArchPchPicRuntime::new(pic, Arc::clone(&self.run));
        *self.built.lock_unpoisoned() = Some(Arc::clone(&runtime));
        runtime
    }
}

/// Device model that keeps the guest-visible PCH-PIC adapter and additionally
/// publishes its per-run runtime under [`LoongArchPchPicRuntimeKey`].
///
/// Everything the model owns is narrow: the device contribution itself and the
/// per-run publication cell. It holds no VM handle and no sleepable state.
pub(crate) struct LoongArchPchPicModel {
    inner: LoongArchPchPicFactory,
    domain: Arc<LoongArchDomainFactory>,
}

impl LoongArchPchPicModel {
    pub(crate) fn new(
        base: usize,
        length: usize,
        run: LoongArchRunBinding,
        output: Arc<dyn PchPicOutputSink>,
    ) -> Arc<Self> {
        let domain = Arc::new(LoongArchDomainFactory {
            run,
            built: Mutex::new(None),
        });
        let domain_trait: Arc<dyn LoongArchInterruptDomainFactory> = domain.clone();
        let inner = LoongArchPchPicFactory::new(base, length, domain_trait, output);
        Arc::new(Self { inner, domain })
    }
}

impl DeviceModel for LoongArchPchPicModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        self.inner.requirements()
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        self.inner.firmware()
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let bundle = self.inner.build(context)?;
        let runtime = self.domain.built.lock_unpoisoned().take().ok_or_else(|| {
            DeviceManagerError::InvalidState {
                operation: "build LoongArch PCH-PIC runtime",
                detail: "the PCH-PIC device did not create its interrupt domain".into(),
            }
        })?;
        bundle.with_service::<LoongArchPchPicRuntimeKey>(runtime)
    }
}

/// One hard-IRQ source slot bound to the exact run that owns its route.
struct PlatformSourceSlot(RawSpinLock<Option<LoongArchRunPort>>);

impl PlatformSourceSlot {
    const fn new() -> Self {
        Self(RawSpinLock::new(None))
    }
}

static PLATFORM_SOURCE_SLOTS: [PlatformSourceSlot; LOONGARCH_MAX_IRQ_COUNT] =
    [const { PlatformSourceSlot::new() }; LOONGARCH_MAX_IRQ_COUNT];

/// Register the platform IRQ injector for LoongArch dynamic hypervisor builds.
pub(crate) fn register_platform_irq_injector() {
    ax_plat::irq::loongarch64_hv::register_virtual_irq_injector(inject_platform_irq);
}

/// Binds every prepared physical source of one run to its exact [`RunId`].
///
/// The routes are registered with the platform only after every slot is bound,
/// so the first hard IRQ can never observe a stale or missing run.
pub(super) fn enter_runtime(
    routes: &[LoongArchPhysicalRoute],
    port: &LoongArchRunPort,
) -> AxVmResult {
    for route in routes {
        if let Err(error) = bind_platform_source(route.physical_irq, port) {
            unbind_run_sources(port.run_id());
            return Err(error);
        }
    }
    for route in routes {
        ax_plat::irq::loongarch64_hv::register_guest_irq_route(
            route.physical_irq,
            port.vm_id(),
            EXTERNAL_TARGET_VCPU,
            route.guest_input,
        );
    }
    Ok(())
}

/// One physical GSI bound to a guest controller input before run admission.
#[derive(Clone, Copy)]
pub(super) struct LoongArchPhysicalRoute {
    pub(super) physical_irq: usize,
    pub(super) guest_input: usize,
}

/// Removes every route and run binding owned by the retiring run.
pub(crate) fn exit_runtime(vm_id: usize, run: RunId) {
    ax_plat::irq::loongarch64_hv::unregister_guest_irq_routes(vm_id);
    unbind_run_sources(run);
}

fn unbind_run_sources(run: RunId) {
    for slot in &PLATFORM_SOURCE_SLOTS {
        let previous = {
            let mut binding = slot.0.lock_irqsave();
            if binding.as_ref().is_some_and(|port| port.run_id() == run) {
                binding.take()
            } else {
                None
            }
        };
        drop(previous);
    }
}

fn bind_platform_source(physical_irq: usize, port: &LoongArchRunPort) -> AxVmResult {
    if physical_irq >= LOONGARCH_MAX_IRQ_COUNT {
        return ax_err!(
            InvalidInput,
            format!("physical IRQ {physical_irq} is outside LoongArch fixed source slots")
        );
    }
    let mut slot = PLATFORM_SOURCE_SLOTS[physical_irq].0.lock_irqsave();
    if let Some(existing) = slot.as_ref()
        && existing.run_id() != port.run_id()
    {
        let existing = existing.run_id();
        drop(slot);
        return ax_err!(
            BadState,
            format!("physical IRQ {physical_irq} is already bound to run {existing:?}")
        );
    }
    let previous = slot.replace(port.clone());
    drop(slot);
    drop(previous);
    Ok(())
}

fn inject_platform_irq(vm_id: usize, vcpu_id: usize, vector: usize, physical_irq: usize) {
    if physical_irq >= LOONGARCH_MAX_IRQ_COUNT {
        warn!("LoongArch physical IRQ {physical_irq} has no fixed source slot");
        return;
    }
    // Clone the Arc identity under the short raw guard, then release it before
    // the publication and its wake/IPI.
    let port = {
        let slot = PLATFORM_SOURCE_SLOTS[physical_irq].0.lock_irqsave();
        slot.as_ref().cloned()
    };
    let Some(port) = port else {
        warn!("LoongArch physical IRQ {physical_irq} arrived without a bound run");
        return;
    };
    if port.vm_id() != vm_id {
        warn!(
            "LoongArch physical IRQ {physical_irq} for VM[{vm_id}] does not match its bound run \
             {:?}",
            port.run_id()
        );
        return;
    }

    // The native controller has already acknowledged the physical source. Keep
    // its identity in the queued value, publish it first, and only then kick.
    let interrupt = QueuedVcpuInterrupt::Physical {
        vector,
        physical_irq,
    };
    if let Err(error) = port.publish_queued_irq(vcpu_id, interrupt) {
        warn!(
            "failed to queue LoongArch IRQ {vector:#x}/physical {physical_irq:#x} for VM[{vm_id}] \
             VCpu[{vcpu_id}]: {error:?}"
        );
    }
}
