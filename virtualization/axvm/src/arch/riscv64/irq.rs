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

use alloc::{collections::BTreeMap, sync::Arc};

use axdevice::{
    ControllerInputId, ControllerRegistration, ControllerRole, DeviceBuildContext, DeviceBundle,
    DeviceFactory, DeviceFactoryRegistry, DeviceManagerError, DeviceManagerResult,
    DeviceRegistration, InterruptControllerId, InterruptEndpoint, InterruptTopology,
    InterruptTriggerMode, IrqError, IrqLine, IrqResult, MmioDeviceAdapter, WiredInterruptInputs,
    WiredIrqInput, WiredIrqRequest, WiredIrqSink,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, VMInterruptMode};
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE,
    PLIC_NUM_SOURCES, VPlicGlobal,
};

use crate::{AxVmError, AxVmResult, ax_err, ax_err_type};

const PLIC_CONTROLLER_ID: InterruptControllerId = InterruptControllerId::new(0);

pub(crate) struct VmArchState {
    external_irq_lines: BTreeMap<usize, IrqLine>,
}

impl VmArchState {
    pub(crate) fn new() -> Self {
        Self {
            external_irq_lines: BTreeMap::new(),
        }
    }

    pub(crate) fn connect_external_irq_lines(
        &mut self,
        topology: &InterruptTopology,
        sources: &[u32],
    ) -> AxVmResult {
        for source in sources {
            let source = *source as usize;
            self.connect_external_irq_line(
                topology,
                source,
                ControllerInputId::new(source),
                InterruptTriggerMode::EdgeTriggered,
            )?;
        }
        Ok(())
    }

    fn connect_external_irq_line(
        &mut self,
        topology: &InterruptTopology,
        source: usize,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        if let Some(existing) = self.external_irq_lines.get(&source) {
            if existing.input() == input && existing.trigger() == trigger {
                return Ok(());
            }
            return ax_err!(
                AlreadyExists,
                format_args!(
                    "external interrupt source {source} is already connected to input {:?}",
                    existing.input()
                )
            );
        }
        let line = topology.connect_irq(WiredIrqRequest::new(input, trigger))?;
        self.external_irq_lines.insert(source, line);
        Ok(())
    }

    fn signal_external_interrupt(&self, source: usize) -> AxVmResult {
        self.external_irq_lines
            .get(&source)
            .ok_or_else(|| {
                ax_err_type!(
                    NotFound,
                    alloc::format!("external interrupt source {source} is not connected")
                )
            })?
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
        let result = if asserted {
            self.vplic.set_pending(input.value())
        } else {
            self.vplic.clear_pending(input.value())
        };
        result.map_err(|error| IrqError::Backend {
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

struct RiscvPlicFactory {
    base_gpa: usize,
    length: usize,
    contexts_num: usize,
    vplic: Arc<VPlicGlobal>,
    interrupt_inputs: Arc<RiscvPlicWiredInputs>,
}

impl DeviceFactory for RiscvPlicFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::PPPTGlobal
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        if config.base_gpa != self.base_gpa
            || config.length != self.length
            || config.cfg_list.as_slice() != [self.contexts_num]
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build virtual PLIC",
                detail: alloc::format!(
                    "factory configuration does not match device '{}'",
                    config.name
                ),
            });
        }
        let mut bundle = DeviceBundle::new();
        bundle.push(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(
            self.vplic.clone(),
        )));
        bundle.push(DeviceRegistration::InterruptController(
            ControllerRegistration::new(PLIC_CONTROLLER_ID, ControllerRole::Default)
                .with_wired_inputs(self.interrupt_inputs.clone()),
        ));
        Ok(bundle)
    }
}

fn validate_vplic_config(config: &EmulatedDeviceConfig) -> AxVmResult<usize> {
    let [contexts_num] = config.cfg_list.as_slice() else {
        return ax_err!(
            InvalidInput,
            format_args!(
                "virtual PLIC device '{}' requires exactly one context-count argument",
                config.name
            )
        );
    };
    let context_end = contexts_num
        .checked_mul(PLIC_CONTEXT_STRIDE)
        .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CTRL_OFFSET))
        .and_then(|offset| offset.checked_add(PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET))
        .and_then(|offset| config.base_gpa.checked_add(offset))
        .ok_or_else(|| ax_err_type!(InvalidInput, "virtual PLIC context range overflow"))?;
    let region_end = config
        .base_gpa
        .checked_add(config.length)
        .ok_or_else(|| ax_err_type!(InvalidInput, "virtual PLIC region range overflow"))?;
    if region_end <= context_end {
        return ax_err!(
            InvalidInput,
            format_args!(
                "virtual PLIC device '{}' range [{:#x}, {:#x}) does not cover {} contexts",
                config.name, config.base_gpa, region_end, contexts_num
            )
        );
    }
    Ok(*contexts_num)
}

pub(crate) fn configure(
    factories: &mut DeviceFactoryRegistry,
    mode: VMInterruptMode,
    configs: &[EmulatedDeviceConfig],
) -> AxVmResult<Arc<InterruptTopology>> {
    let topology = Arc::new(InterruptTopology::new(mode));
    let mut vplic_configs = configs
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::PPPTGlobal);
    let Some(config) = vplic_configs.next() else {
        return Ok(topology);
    };
    if vplic_configs.next().is_some() {
        return ax_err!(
            AlreadyExists,
            "a VM can register only one virtual PLIC global controller"
        );
    }
    if mode == VMInterruptMode::NoIrq {
        return ax_err!(
            InvalidInput,
            "a VM configured with interrupt_mode=no_irq cannot register a virtual PLIC"
        );
    }

    let contexts_num = validate_vplic_config(config)?;
    let vplic = Arc::new(
        VPlicGlobal::new(config.base_gpa.into(), Some(config.length), contexts_num)
            .map_err(AxVmError::invalid_config)?,
    );
    let interrupt_inputs = Arc::new(RiscvPlicWiredInputs {
        sink: Arc::new(RiscvPlicIrqSink {
            vplic: vplic.clone(),
        }),
    });
    factories.register(Arc::new(RiscvPlicFactory {
        base_gpa: config.base_gpa,
        length: config.length,
        contexts_num,
        vplic,
        interrupt_inputs,
    }))?;

    Ok(topology)
}
