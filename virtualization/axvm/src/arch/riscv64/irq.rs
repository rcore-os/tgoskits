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

use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use axdevice::{
    AxVmDevices, ControllerInputId, ControllerRegistration, ControllerRole, DeviceBundle,
    DeviceRegistration, InterruptControllerId, InterruptEndpoint, InterruptEndpointRegistration,
    InterruptPlanAuthority, InterruptSharing, InterruptTopology, InterruptTriggerMode, IrqError,
    IrqLine, IrqResult, MmioDeviceAdapter, WiredInterruptInputs, WiredIrqInput, WiredIrqRequest,
    WiredIrqSink,
};
use riscv_vplic::{PLIC_NUM_SOURCES, VPlicGlobal};

use crate::{
    AxVmError, AxVmResult, ax_err, ax_err_type,
    machine::{HostInterruptResource, InterruptControllerPlan, RiscvPlicPlan, VmMachinePlan},
};

const PLIC_CONTROLLER_ID: InterruptControllerId = InterruptControllerId::new(0);

pub(crate) struct VmArchState {
    external_irq_routes: BTreeMap<usize, ExternalIrqRoute>,
}

struct ExternalIrqRoute {
    line: IrqLine,
    _registration: InterruptEndpointRegistration,
}

impl VmArchState {
    pub(crate) fn new() -> Self {
        Self {
            external_irq_routes: BTreeMap::new(),
        }
    }

    pub(crate) fn connect_external_irq_lines(
        &mut self,
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        sources: &[HostInterruptResource],
    ) -> AxVmResult {
        for interrupt in sources {
            let source = interrupt.input().value();
            self.connect_external_irq_line(topology, authority, source, interrupt.input())?;
        }
        Ok(())
    }

    fn connect_external_irq_line(
        &mut self,
        topology: &InterruptTopology,
        authority: &InterruptPlanAuthority,
        source: usize,
        input: ControllerInputId,
    ) -> AxVmResult {
        let trigger = InterruptTriggerMode::EdgeTriggered;
        if let Some(existing) = self.external_irq_routes.get(&source) {
            if existing.line.input() == input && existing.line.trigger() == trigger {
                return Ok(());
            }
            return ax_err!(
                AlreadyExists,
                format_args!(
                    "external interrupt source {source} is already connected to input {:?}",
                    existing.line.input()
                )
            );
        }
        let claim = authority.claim_wired(
            topology,
            WiredIrqRequest::new(input, trigger, InterruptSharing::Exclusive),
        )?;
        let (line, registration) = topology.connect_irq(claim)?.into_parts();
        self.external_irq_routes.insert(
            source,
            ExternalIrqRoute {
                line,
                _registration: registration,
            },
        );
        Ok(())
    }

    fn signal_external_interrupt(&self, source: usize) -> AxVmResult {
        self.external_irq_routes
            .get(&source)
            .ok_or_else(|| {
                ax_err_type!(
                    NotFound,
                    alloc::format!("external interrupt source {source} is not connected")
                )
            })?
            .line
            .pulse()?;
        Ok(())
    }
}

pub(crate) fn signal_external_interrupt(vm: &crate::AxVM, source: usize) -> AxVmResult {
    match vm.status() {
        crate::VmStatus::Running | crate::VmStatus::Paused => vm.with_resources_mut(|resources| {
            resources.arch_state_mut().signal_external_interrupt(source)
        }),
        status => ax_err!(
            BadState,
            alloc::format!("VM[{}] cannot accept IRQ in {status:?}", vm.id())
        ),
    }
}

struct RiscvPlicIrqSink {
    vplic: Arc<VPlicGlobal>,
}

impl WiredIrqSink for RiscvPlicIrqSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        self.vplic
            .set_source_level(input.value(), asserted)
            .map_err(|error| IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: PLIC_CONTROLLER_ID,
                    input,
                },
                operation: "set vPLIC line level",
                detail: alloc::format!("{error}"),
            })
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        self.vplic
            .set_pending(input.value())
            .map_err(|error| IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: PLIC_CONTROLLER_ID,
                    input,
                },
                operation: "pulse vPLIC line",
                detail: alloc::format!("{error}"),
            })
    }
}

struct RiscvPlicWiredInputs {
    sink: Arc<RiscvPlicIrqSink>,
}

impl WiredInterruptInputs for RiscvPlicWiredInputs {
    fn input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        if input.value() == 0 || input.value() >= PLIC_NUM_SOURCES {
            return Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: PLIC_CONTROLLER_ID,
                    input,
                },
                operation: "connect vPLIC source",
                detail: alloc::format!(
                    "PLIC source must be in 1..{PLIC_NUM_SOURCES}, got {}",
                    input.value()
                ),
            });
        }
        Ok(WiredIrqInput::new(
            PLIC_CONTROLLER_ID,
            input,
            trigger,
            self.sink.clone(),
        ))
    }
}

/// Runtime PLIC capabilities built from the immutable machine plan.
pub(crate) struct PreparedPlic {
    vplic: Arc<VPlicGlobal>,
    interrupt_inputs: Arc<RiscvPlicWiredInputs>,
}

impl PreparedPlic {
    pub(crate) fn from_machine_plan(plan: &VmMachinePlan) -> AxVmResult<Self> {
        let layout = plic_layout(plan)?;
        let base = usize::try_from(layout.mmio().base())
            .map_err(|_| AxVmError::invalid_config("virtual PLIC base exceeds usize"))?;
        let size = usize::try_from(layout.mmio().size())
            .map_err(|_| AxVmError::invalid_config("virtual PLIC size exceeds usize"))?;
        let vplic = Arc::new(
            VPlicGlobal::new(base.into(), Some(size), layout.context_count())
                .map_err(AxVmError::invalid_config)?,
        );
        vplic.restrict_to_assigned_sources();
        for source in planned_plic_sources(plan, layout.source_count())? {
            vplic
                .assign_source(source)
                .map_err(AxVmError::invalid_config)?;
        }
        let interrupt_inputs = Arc::new(RiscvPlicWiredInputs {
            sink: Arc::new(RiscvPlicIrqSink {
                vplic: vplic.clone(),
            }),
        });
        Ok(Self {
            vplic,
            interrupt_inputs,
        })
    }

    pub(crate) fn register(
        &self,
        devices: &mut AxVmDevices,
        topology: &InterruptTopology,
    ) -> AxVmResult {
        let mut bundle = DeviceBundle::new();
        bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
            self.vplic.clone(),
        )));
        bundle.push(DeviceRegistration::InterruptController(
            ControllerRegistration::new(PLIC_CONTROLLER_ID, ControllerRole::Default)
                .with_wired_inputs(self.interrupt_inputs.clone()),
        ));
        devices.register_bundle_with_topology(bundle, topology)?;
        Ok(())
    }
}

fn planned_plic_sources(plan: &VmMachinePlan, source_count: u32) -> AxVmResult<BTreeSet<usize>> {
    let mut sources = BTreeSet::new();
    for source in plan
        .assigned_host_interrupts()
        .iter()
        .map(HostInterruptResource::input_u32)
        .chain(
            plan.virtual_devices()
                .iter()
                .flat_map(|device| device.interrupts())
                .map(crate::machine::ResolvedInterrupt::id),
        )
    {
        if source == 0 || source > source_count {
            return Err(AxVmError::invalid_config(alloc::format!(
                "planned PLIC source {source} is outside 1..={source_count}"
            )));
        }
        sources
            .insert(usize::try_from(source).map_err(|_| {
                AxVmError::invalid_config("planned PLIC source does not fit usize")
            })?);
    }
    Ok(sources)
}

fn plic_layout(plan: &VmMachinePlan) -> AxVmResult<&RiscvPlicPlan> {
    match plan.interrupt_controller() {
        Some(InterruptControllerPlan::RiscvPlic(layout)) => Ok(layout),
        Some(_) => Err(AxVmError::invalid_config(
            "RISC-V VM machine plan contains a controller for another architecture",
        )),
        None => Err(AxVmError::invalid_config(
            "RISC-V VM machine plan has no mandatory PLIC controller",
        )),
    }
}
