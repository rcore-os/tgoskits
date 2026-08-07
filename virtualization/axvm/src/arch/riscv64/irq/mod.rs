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

use std::{collections::BTreeMap, sync::Arc, vec::Vec};

use ax_std::os::arceos::sync::IrqSafeMutex;
use axdevice::*;
use axdevice_base::*;
use axvm_types::{GuestPhysAddr, InterruptTriggerMode};
use riscv_vplic::*;

use crate::{AxVmError, AxVmResult, ax_err, ax_err_type, irq::deferred::DeferredVcpuKick};

mod physical;

/// Typed VM-local access to vPLIC state and deferred wake lifecycle.
pub(crate) struct RiscvPlicRuntimeKey;

impl ServiceKey for RiscvPlicRuntimeKey {
    type Service = RiscvPlicRuntime;

    const NAME: &'static str = "riscv-vplic-runtime";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// VM-owned RISC-V interrupt-controller runtime.
///
/// `VPlicGlobal` is the sole owner of pending, active, enable, priority,
/// threshold, and level state. The deferred kick bitmap carries only the
/// identity of vCPUs that must re-evaluate that state.
pub(crate) struct RiscvPlicRuntime {
    vplic: Arc<VPlicGlobal>,
    sink: Arc<RiscvPlicWiredSink>,
    inputs: IrqSafeMutex<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
    kick: Arc<DeferredVcpuKick>,
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
                    "RISC-V VM has {vcpu_count} vCPUs, but deferred IRQ wake supports at most {}",
                    usize::BITS
                )
            );
        }
        let kick = DeferredVcpuKick::new(vm_id);
        let sink = Arc::new(RiscvPlicWiredSink {
            vplic: vplic.clone(),
            kick: kick.clone(),
            vcpu_count,
        });
        let physical = physical::PhysicalIrqBridge::new(
            vm_id,
            vplic.clone(),
            kick.clone(),
            vcpu_count,
            physical_irqs,
            physical_target_cpu,
        )?;
        Ok(Arc::new(Self {
            vplic,
            sink,
            inputs: IrqSafeMutex::new(BTreeMap::new()),
            kick,
            physical,
            vcpu_count,
        }))
    }

    pub(crate) fn activate(self: &Arc<Self>) -> AxVmResult {
        self.kick.start();
        if let Err(error) = self.physical.start() {
            self.kick.stop();
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn deactivate(&self) -> AxVmResult {
        let result = self.physical.stop();
        self.kick.stop();
        result
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
            .context_has_deliverable_irq(context_id)
            .map_err(|error| AxVmError::interrupt("derive RISC-V VSEIP state", error))
    }

    #[cfg(test)]
    fn take_pending_kicks_for_test(&self) -> usize {
        self.kick.take_pending_for_test()
    }
}

impl Drop for RiscvPlicRuntime {
    fn drop(&mut self) {
        let _ = self.physical.stop();
        self.kick.stop();
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
                    controller: self.id(),
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
                        controller: self.id(),
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
        let registered = WiredIrqInput::new(self.id(), input, trigger, sink);
        inputs.insert(source, (trigger, registered.clone()));
        Ok(registered)
    }
}

struct RiscvPlicWiredSink {
    vplic: Arc<VPlicGlobal>,
    kick: Arc<DeferredVcpuKick>,
    vcpu_count: usize,
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

    fn publish_vcpu_kicks(&self, input: ControllerInputId) -> IrqResult {
        for vcpu_id in 0..self.vcpu_count {
            self.kick.publish_from_irq(vcpu_id).map_err(|error| {
                Self::backend_error(input, "publish deferred RISC-V vCPU kick", error)
            })?;
        }
        Ok(())
    }
}

impl WiredIrqSink for RiscvPlicWiredSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        self.vplic
            .set_irq_line_level(input.value(), asserted)
            .map_err(|error| Self::backend_error(input, "set RISC-V vPLIC line level", error))?;
        self.publish_vcpu_kicks(input)
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.vplic
            .set_pending(input.value())
            .map_err(|error| Self::backend_error(input, "pulse RISC-V vPLIC input", error))?;
        self.publish_vcpu_kicks(input)
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

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let addr = GuestPhysAddr::from_usize(access.addr as usize);
        if access.is_read {
            self.runtime
                .vplic
                .read_register(addr, access.width)
                .map(|value| BusResponse::Read {
                    value: value as u64,
                })
        } else {
            let completion = self.runtime.vplic.write_register_with_completion(
                addr,
                access.width,
                access.data as usize,
            )?;
            if let Some(completion) = completion {
                self.runtime.physical.complete_source(completion.source());
            }
            Ok(BusResponse::Write)
        }
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
            ControllerRegistration::new(runtime.id(), controller),
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

        let first = runtime
            .wired_input(input, InterruptTriggerMode::LevelTriggered)
            .unwrap();
        let second = runtime
            .wired_input(input, InterruptTriggerMode::LevelTriggered)
            .unwrap();

        assert_eq!(first.input(), second.input());
        assert!(
            runtime
                .wired_input(input, InterruptTriggerMode::EdgeTriggered)
                .is_err()
        );
    }

    #[test]
    fn level_transition_updates_controller_state_and_only_publishes_vcpu_bits() {
        let runtime = runtime();
        let line = runtime
            .wired_input(
                ControllerInputId::new(10),
                InterruptTriggerMode::LevelTriggered,
            )
            .unwrap()
            .connect()
            .unwrap();

        line.assert().unwrap();
        assert!(runtime.vplic.is_pending(10).unwrap());
        assert_eq!(runtime.take_pending_kicks_for_test(), 0b11);

        line.deassert().unwrap();
        assert!(!runtime.vplic.is_pending(10).unwrap());
        assert_eq!(runtime.take_pending_kicks_for_test(), 0b11);
    }
}
