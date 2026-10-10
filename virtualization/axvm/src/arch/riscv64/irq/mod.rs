// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! RISC-V virtual PLIC interrupt backend.

use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
    vec::Vec,
};

use ax_std::os::arceos::sync::RawSpinLock;
use ax_sync::Mutex;
use axdevice::*;
use axdevice_base::*;
use axvm_types::{GuestPhysAddr, InterruptTriggerMode};
use riscv_vplic::*;

use crate::{
    AxVmError, AxVmResult, ax_err, ax_err_type,
    irq::model::{
        InterruptControllerEndpoint, InterruptControllerOwner, InterruptSourceId, RunEpoch,
        SourceEvent,
    },
    services::{RunSignals, SignalError},
};

mod physical;

/// Run-bound vCPU wake target captured for one execution period.
///
/// The controller publishes its pending state before calling this binding, so
/// the binding carries no interrupt identity and never looks up the VM. The
/// owner installs the run-bound target before IRQ input is enabled and clears
/// it only after input is quiesced.
pub(super) struct RunKickBinding {
    signals: RawSpinLock<Option<Arc<RunSignals>>>,
}

impl RunKickBinding {
    fn new() -> Self {
        Self {
            signals: RawSpinLock::new(None),
        }
    }

    fn bind(&self, signals: Arc<RunSignals>) {
        *self.signals.lock_irqsave() = Some(signals);
    }

    fn clear(&self) {
        *self.signals.lock_irqsave() = None;
    }

    /// Clones the published target without holding the raw guard across a wake.
    fn current(&self) -> Option<Arc<RunSignals>> {
        self.signals.lock_irqsave().as_ref().cloned()
    }

    /// Wakes one vCPU from task context after controller state is visible.
    fn kick(&self, vcpu_id: usize) -> Result<(), SignalError> {
        match self.current() {
            Some(signals) => signals.kick(vcpu_id),
            None => Err(SignalError::Closed),
        }
    }
}

/// Typed VM-local access to vPLIC state and run-bound wake lifecycle.
pub(crate) struct RiscvPlicRuntimeKey;

impl ServiceKey for RiscvPlicRuntimeKey {
    type Service = RiscvPlicRuntime;

    const NAME: &'static str = "riscv-vplic-runtime";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// VM-owned RISC-V interrupt-controller runtime.
///
/// `VPlicGlobal` is the sole owner of pending, active, enable, priority,
/// threshold, and level state. A wake carries only the identity of a vCPU that
/// must rederive its VSEIP from that state and is published through the
/// run-bound target captured at activation.
pub(crate) struct RiscvPlicRuntime {
    vplic: Arc<VPlicGlobal>,
    sink: Arc<RiscvPlicWiredSink>,
    /// Task-side registration table. Hard IRQ paths use the fixed physical
    /// ingress and never acquire this sleepable mutex.
    inputs: Mutex<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
    kick: Arc<RunKickBinding>,
    physical: Arc<physical::PhysicalIrqBridge>,
    vcpu_count: usize,
}

impl RiscvPlicRuntime {
    fn new(
        vm_id: usize,
        vcpu_count: usize,
        vplic: Arc<VPlicGlobal>,
        physical_irqs: &[crate::config::PassthroughInterrupt],
        physical_target_cpu: usize,
    ) -> AxVmResult<Arc<Self>> {
        if vcpu_count == 0 {
            return ax_err!(InvalidInput, "a RISC-V VM must contain at least one vCPU");
        }
        if vcpu_count > usize::BITS as usize {
            return ax_err!(
                Unsupported,
                std::format!(
                    "RISC-V VM has {vcpu_count} vCPUs, but the run-bound wake bitmap supports at \
                     most {}",
                    usize::BITS
                )
            );
        }
        let kick = Arc::new(RunKickBinding::new());
        let physical = physical::PhysicalIrqBridge::new(
            vm_id,
            vplic.clone(),
            kick.clone(),
            vcpu_count,
            physical_irqs,
            physical_target_cpu,
        )?;
        Ok(Arc::new_cyclic(|runtime| Self {
            vplic,
            sink: Arc::new(RiscvPlicWiredSink {
                runtime: runtime.clone(),
            }),
            inputs: Mutex::new(BTreeMap::new()),
            kick,
            physical,
            vcpu_count,
        }))
    }

    /// Publishes the run-bound wake target and enables physical IRQ input.
    ///
    /// The target is installed before IRQ input is enabled, so the first
    /// controller publication can never observe a stale run.
    pub(crate) fn activate(self: &Arc<Self>, signals: Arc<RunSignals>) -> AxVmResult {
        self.kick.bind(signals);
        if let Err(error) = self.physical.start() {
            self.kick.clear();
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn deactivate(&self) -> AxVmResult {
        let physical = self.physical.stop();
        // Physical input is quiesced before the wake target is retired, so no
        // publication can observe a cleared binding.
        self.kick.clear();
        physical
    }

    /// Extracts the narrow delivery capability carried by an entry or exit.
    ///
    /// The returned port reaches only the VM-local vPLIC controller, never the
    /// physical IRQ bridge, run services, or the sleeping registration locks,
    /// so a hardware entry can rederive VSEIP without acquiring a sleepable
    /// lock or querying the complete VM.
    pub(crate) fn delivery_port(&self) -> VplicDeliveryPort {
        VplicDeliveryPort {
            vplic: Arc::clone(&self.vplic),
            vcpu_count: self.vcpu_count,
        }
    }

    fn current_epoch(&self) -> AxVmResult<RunEpoch> {
        self.kick
            .current()
            .map(|signals| signals.epoch())
            .ok_or_else(|| AxVmError::interrupt("submit RISC-V vPLIC event", "no active run"))
    }

    fn validate_source(&self, source: InterruptSourceId) -> AxVmResult<usize> {
        let controller = <Self as VirtualInterruptController>::id(self);
        if source.controller != controller {
            return ax_err!(
                InvalidInput,
                std::format!(
                    "RISC-V vPLIC event targets controller {:?}, expected {:?}",
                    source.controller,
                    controller
                )
            );
        }
        let source_id = usize::try_from(source.source).map_err(|_| {
            AxVmError::invalid_input("submit RISC-V vPLIC event", "source does not fit usize")
        })?;
        if source_id == 0 || source_id >= PLIC_NUM_SOURCES {
            return ax_err!(
                InvalidInput,
                std::format!("RISC-V vPLIC source {source_id} is outside 1..{PLIC_NUM_SOURCES}")
            );
        }
        Ok(source_id)
    }
}

/// Narrow read-only vPLIC delivery capability owned by an entry and its exits.
///
/// It exposes only the two controller-derived queries the RISC-V adapter needs
/// and owns nothing but the immutable vPLIC controller plus the fixed vCPU
/// count, so no sleeping lock is reachable through it.
#[derive(Clone)]
pub(crate) struct VplicDeliveryPort {
    vplic: Arc<VPlicGlobal>,
    vcpu_count: usize,
}

impl VplicDeliveryPort {
    /// Returns whether `addr` belongs to this VM's vPLIC register window.
    pub(crate) fn contains_guest_addr(&self, addr: GuestPhysAddr) -> bool {
        let base = self.vplic.addr.as_usize();
        let end = base.saturating_add(self.vplic.size);
        let addr = addr.as_usize();
        addr >= base && addr < end
    }

    /// Returns the controller-derived VSEIP state for one vCPU.
    pub(crate) fn vcpu_has_deliverable_irq(&self, vcpu_id: usize) -> AxVmResult<bool> {
        if vcpu_id >= self.vcpu_count {
            return ax_err!(
                InvalidInput,
                std::format!(
                    "RISC-V vCPU {vcpu_id} is outside the configured range 0..{}",
                    self.vcpu_count
                )
            );
        }
        let context_id = vcpu_id
            .checked_mul(2)
            .and_then(|context| context.checked_add(1))
            .ok_or_else(|| ax_err_type!(InvalidInput, "RISC-V vPLIC context ID overflow"))?;
        self.vplic
            .context_deliverable(context_id)
            .map_err(|error| AxVmError::interrupt("derive RISC-V VSEIP state", error))
    }
}

impl Drop for RiscvPlicRuntime {
    fn drop(&mut self) {
        let _ = self.physical.stop();
        self.kick.clear();
    }
}

impl VirtualInterruptController for RiscvPlicRuntime {
    fn id(&self) -> InterruptControllerId {
        InterruptControllerId::new(0)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        let source = input.value();
        if source == 0 || source >= PLIC_NUM_SOURCES {
            return Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: <Self as VirtualInterruptController>::id(self),
                    input,
                },
                operation: "open RISC-V vPLIC input",
                detail: std::format!(
                    "source {source} is outside the valid range 1..{PLIC_NUM_SOURCES}"
                ),
            });
        }

        let mut inputs = self.inputs.lock();
        if let Some((registered_trigger, registered)) = inputs.get(&source) {
            if *registered_trigger != trigger {
                return Err(IrqError::InvalidInput {
                    endpoint: InterruptEndpoint::Wired {
                        controller: <Self as VirtualInterruptController>::id(self),
                        input,
                    },
                    operation: "open RISC-V vPLIC input",
                    detail: std::format!(
                        "source {source} is already registered as {registered_trigger:?}"
                    ),
                });
            }
            return Ok(registered.clone());
        }

        let sink: Arc<dyn WiredIrqSink> = self.sink.clone();
        let registered = WiredIrqInput::new(
            <Self as VirtualInterruptController>::id(self),
            input,
            trigger,
            sink,
        );
        inputs.insert(source, (trigger, registered.clone()));
        Ok(registered)
    }
}

impl InterruptControllerEndpoint for RiscvPlicRuntime {
    type Error = AxVmError;

    fn id(&self) -> InterruptControllerId {
        <Self as VirtualInterruptController>::id(self)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        <Self as VirtualInterruptController>::wired_input(self, input, trigger)
    }

    fn submit(&self, event: SourceEvent) -> AxVmResult {
        let epoch = match event {
            SourceEvent::Pulse { epoch, .. }
            | SourceEvent::Level { epoch, .. }
            | SourceEvent::Eoi { epoch, .. } => epoch,
        };
        let current = self.current_epoch()?;
        if epoch != current {
            return Err(AxVmError::StaleRun {
                expected: epoch.run(),
                current: Some(current.run()),
            });
        }
        self.kick
            .current()
            .ok_or_else(|| AxVmError::interrupt("submit RISC-V interrupt", "no active run"))?
            .publish_controller_event(event)
            .map_err(|error| AxVmError::interrupt("queue RISC-V interrupt event", error))
    }
}

impl InterruptControllerOwner for RiscvPlicRuntime {
    type Error = AxVmError;

    fn apply_source(&self, event: SourceEvent) -> AxVmResult {
        let epoch = match event {
            SourceEvent::Pulse { epoch, .. }
            | SourceEvent::Level { epoch, .. }
            | SourceEvent::Eoi { epoch, .. } => epoch,
        };
        let current = self.current_epoch()?;
        if epoch != current {
            return Err(AxVmError::StaleRun {
                expected: epoch.run(),
                current: Some(current.run()),
            });
        }

        match event {
            SourceEvent::Pulse { source, .. } => {
                let source_id = self.validate_source(source)?;
                self.vplic
                    .set_pending(source_id)
                    .map_err(|error| AxVmError::interrupt("pulse RISC-V vPLIC input", error))?;
            }
            SourceEvent::Level {
                source, asserted, ..
            } => {
                let source_id = self.validate_source(source)?;
                self.vplic
                    .set_irq_line_level(source_id, asserted)
                    .map_err(|error| AxVmError::interrupt("set RISC-V vPLIC line level", error))?;
            }
            SourceEvent::Eoi { token, .. } => {
                if token.target.run != epoch.run {
                    return Err(AxVmError::invalid_input(
                        "complete RISC-V vPLIC source",
                        "delivery token belongs to a different VM run",
                    ));
                }
                if token.target.vcpu_id >= self.vcpu_count {
                    return Err(AxVmError::invalid_input(
                        "complete RISC-V vPLIC source",
                        "delivery token targets an unknown vCPU",
                    ));
                }
                let source_id = self.validate_source(token.source)?;
                self.vplic
                    .complete_source(source_id)
                    .map_err(|error| AxVmError::interrupt("complete RISC-V vPLIC source", error))?;
            }
        }

        // Canonical PLIC state is published before waking any vCPU. The wake
        // path only carries the target identity and never acquires the PLIC
        // mutex from an IRQ/raw guard.
        for vcpu_id in 0..self.vcpu_count {
            if let Err(error) = self.kick.kick(vcpu_id) {
                trace!("RISC-V vPLIC event could not wake vCPU {vcpu_id}: {error:?}");
            }
        }
        Ok(())
    }
}

struct RiscvPlicWiredSink {
    runtime: Weak<RiscvPlicRuntime>,
}

impl RiscvPlicWiredSink {
    fn endpoint(input: ControllerInputId) -> InterruptEndpoint {
        InterruptEndpoint::Wired {
            controller: InterruptControllerId::new(0),
            input,
        }
    }

    fn backend_error(
        input: ControllerInputId,
        operation: &'static str,
        error: impl std::fmt::Display,
    ) -> IrqError {
        IrqError::Backend {
            endpoint: Self::endpoint(input),
            operation,
            detail: std::format!("{error}"),
        }
    }
}

impl WiredIrqSink for RiscvPlicWiredSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        let runtime = self.runtime.upgrade().ok_or_else(|| {
            Self::backend_error(input, "set RISC-V vPLIC line level", "controller is closed")
        })?;
        if let Some(epoch) = runtime.kick.current().map(|signals| signals.epoch()) {
            InterruptControllerEndpoint::submit(
                runtime.as_ref(),
                SourceEvent::Level {
                    epoch,
                    source: InterruptSourceId::new(
                        <RiscvPlicRuntime as VirtualInterruptController>::id(runtime.as_ref()),
                        input.value() as u32,
                        None,
                    ),
                    asserted,
                },
            )
            .map_err(|error| Self::backend_error(input, "set RISC-V vPLIC line level", error))
        } else {
            // Wiring may be prepared before the first run. There is no epoch
            // to attach in that state, so publish only the canonical owner
            // state and defer wake-up until activation.
            runtime
                .vplic
                .set_irq_line_level(input.value(), asserted)
                .map(|_| ())
                .map_err(|error| Self::backend_error(input, "set RISC-V vPLIC line level", error))
        }
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        let runtime = self.runtime.upgrade().ok_or_else(|| {
            Self::backend_error(input, "pulse RISC-V vPLIC input", "controller is closed")
        })?;
        if let Some(epoch) = runtime.kick.current().map(|signals| signals.epoch()) {
            InterruptControllerEndpoint::submit(
                runtime.as_ref(),
                SourceEvent::Pulse {
                    epoch,
                    source: InterruptSourceId::new(
                        <RiscvPlicRuntime as VirtualInterruptController>::id(runtime.as_ref()),
                        input.value() as u32,
                        None,
                    ),
                },
            )
            .map_err(|error| Self::backend_error(input, "pulse RISC-V vPLIC input", error))
        } else {
            runtime
                .vplic
                .set_pending(input.value())
                .map_err(|error| Self::backend_error(input, "pulse RISC-V vPLIC input", error))
        }
    }
}

struct RiscvPlicFactory {
    vm_id: usize,
    vcpu_count: usize,
    base: usize,
    length: usize,
    contexts_num: usize,
    physical_irqs: Vec<crate::config::PassthroughInterrupt>,
    physical_target_cpu: usize,
}

struct RiscvPlicDevice {
    runtime: Arc<RiscvPlicRuntime>,
}

impl Device for RiscvPlicDevice {
    fn name(&self) -> &str {
        "riscv-vplic"
    }

    fn resources(&self) -> &[axdevice_base::Resource] {
        self.runtime.vplic.resources()
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        // The vPLIC remains the sole owner of pending, active, enable,
        // priority, and threshold state. The accessing vCPU rederives VSEIP
        // from this controller on its next bound guest entry.
        self.runtime
            .vplic
            .read_register(
                GuestPhysAddr::from_usize(access.address() as usize),
                access.width(),
            )
            .map(|value| value as u64)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let completion = self.runtime.vplic.write_register_with_completion(
            GuestPhysAddr::from_usize(access.address() as usize),
            access.width(),
            value as usize,
        )?;
        if let Some(completion) = completion {
            self.runtime.physical.complete_source(completion.source());
        }
        Ok(())
    }
}

impl DeviceModel for RiscvPlicFactory {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new().with_mmio(
            ResourceSlot::new("registers")?,
            self.length as u64,
            1,
            ResourceRequest::Fixed(self.base as u64),
        )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::interfaces(
            Some(std::vec![FdtContributionSpec::InterruptController {
                controller: axdevice_base::InterruptControllerId::new(0),
                node: FdtNodeSpec::new("plic")
                    .with_compatible("riscv,plic0")
                    .with_register(
                        ResourceSlot::new("registers").expect("static PLIC slot is valid"),
                    ),
            }]),
            None,
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = context.mmio(&ResourceSlot::new("registers")?)?;
        if base != self.base as u64 || length != self.length as u64 {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build RISC-V virtual PLIC",
                detail: "planned MMIO range differs from the machine descriptor".into(),
            });
        }
        let base = usize::try_from(base).map_err(|_| DeviceManagerError::InvalidConfig {
            operation: "build RISC-V virtual PLIC",
            detail: "planned MMIO base does not fit the target address width".into(),
        })?;
        let length = usize::try_from(length).map_err(|_| DeviceManagerError::InvalidConfig {
            operation: "build RISC-V virtual PLIC",
            detail: "planned MMIO length does not fit the target address width".into(),
        })?;
        let vplic = Arc::new(
            VPlicGlobal::new(base.into(), Some(length), self.contexts_num).map_err(|error| {
                DeviceManagerError::InvalidConfig {
                    operation: "build RISC-V virtual PLIC",
                    detail: std::format!("{error}"),
                }
            })?,
        );
        let runtime = RiscvPlicRuntime::new(
            self.vm_id,
            self.vcpu_count,
            vplic,
            &self.physical_irqs,
            self.physical_target_cpu,
        )
        .map_err(|error| DeviceManagerError::InvalidConfig {
            operation: "build RISC-V virtual PLIC",
            detail: std::format!("{error}"),
        })?;
        let device: Arc<dyn Device> = Arc::new(RiscvPlicDevice {
            runtime: runtime.clone(),
        });
        let controller: Arc<dyn VirtualInterruptController> = runtime.clone();
        let mut bundle = DeviceBundle::from_registration(DeviceRegistration::Device(device))
            .with_service::<RiscvPlicRuntimeKey>(runtime.clone())?;
        bundle.push(DeviceRegistration::InterruptController(
            ControllerRegistration::new(
                <RiscvPlicRuntime as VirtualInterruptController>::id(&runtime),
                controller,
            ),
        ));
        Ok(bundle)
    }
}

fn validate_vplic_layout(base: usize, length: usize, contexts_num: usize) -> AxVmResult {
    let context_end = contexts_num
        .checked_mul(PLIC_CONTEXT_STRIDE)
        .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CTRL_OFFSET))
        .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET))
        .and_then(|offset| base.checked_add(offset))
        .ok_or_else(|| ax_err_type!(InvalidInput, "virtual PLIC context range overflow"))?;
    let region_end = base
        .checked_add(length)
        .ok_or_else(|| ax_err_type!(InvalidInput, "virtual PLIC region range overflow"))?;
    if region_end <= context_end {
        return ax_err!(
            InvalidInput,
            format_args!(
                "virtual PLIC range [{base:#x}, {region_end:#x}) does not cover {contexts_num} \
                 contexts"
            )
        );
    }
    Ok(())
}

/// Creates the canonical vPLIC and registers its only construction path.
pub(crate) fn model(
    vm_id: usize,
    vcpu_count: usize,
    base: usize,
    length: usize,
    physical_irqs: &[crate::config::PassthroughInterrupt],
    physical_target_cpu: usize,
) -> AxVmResult<Arc<dyn DeviceModel>> {
    let expected_contexts = vcpu_count
        .checked_mul(2)
        .ok_or_else(|| ax_err_type!(InvalidInput, "RISC-V vPLIC context count overflow"))?;
    validate_vplic_layout(base, length, expected_contexts)?;
    Ok(Arc::new(RiscvPlicFactory {
        vm_id,
        vcpu_count,
        base,
        length,
        contexts_num: expected_contexts,
        physical_irqs: physical_irqs.to_vec(),
        physical_target_cpu,
    }))
}

struct RiscvPhysicalPlicIngress;

#[ax_crate_interface::impl_interface]
impl ax_plat::irq::riscv64_hv::RiscvHvIrqSink for RiscvPhysicalPlicIngress {
    fn publish_physical_plic_claim(source: u32) -> bool {
        physical::publish_physical_claim_from_irq(source)
    }
}

#[cfg(test)]
mod tests {
    use axdevice_base::{ControllerInputId, InterruptTriggerMode};
    use axvm_types::GuestPhysAddr;

    use super::*;

    fn runtime() -> Arc<RiscvPlicRuntime> {
        let vplic = Arc::new(
            VPlicGlobal::new(GuestPhysAddr::from(0x0c00_0000), Some(0x60_0000), 4).unwrap(),
        );
        RiscvPlicRuntime::new(7, 2, vplic, &[], 0).unwrap()
    }

    #[test]
    fn repeated_wired_input_claims_share_state_and_reject_trigger_conflicts() {
        let runtime = runtime();
        let input = ControllerInputId::new(10);

        let first = InterruptControllerEndpoint::wired_input(
            &*runtime,
            input,
            InterruptTriggerMode::LevelTriggered,
        )
        .unwrap();
        let second = InterruptControllerEndpoint::wired_input(
            &*runtime,
            input,
            InterruptTriggerMode::LevelTriggered,
        )
        .unwrap();

        assert_eq!(first.input(), second.input());
        assert!(
            InterruptControllerEndpoint::wired_input(
                &*runtime,
                input,
                InterruptTriggerMode::EdgeTriggered,
            )
            .is_err()
        );
    }

    #[test]
    fn level_transition_updates_controller_state_without_a_bound_run() {
        let runtime = runtime();
        let line = InterruptControllerEndpoint::wired_input(
            &*runtime,
            ControllerInputId::new(10),
            InterruptTriggerMode::LevelTriggered,
        )
        .unwrap()
        .connect()
        .unwrap();

        // The controller is the sole owner of line and pending state. A wake
        // with no bound run must not turn a device transition into an error.
        line.assert().unwrap();
        assert!(runtime.vplic.is_pending(10).unwrap());

        line.deassert().unwrap();
        assert!(!runtime.vplic.is_pending(10).unwrap());
    }
}
