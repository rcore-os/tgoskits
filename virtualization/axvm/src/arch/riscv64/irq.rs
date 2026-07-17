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

use alloc::sync::Arc;

use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceManagerError,
    DeviceManagerResult, DeviceRegistration, MmioDeviceAdapter,
};
use axdevice_base::{IrqError, IrqLineId, IrqResult, IrqSink};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, VMInterruptMode};
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE, VPlicGlobal,
};

use crate::{
    AxVmError, AxVmResult, ax_err, ax_err_type,
    irq::{
        InterruptFabric,
        forwarding::{ControllerIrqRegistry, PhysicalIrqRoute},
    },
};

const PLIC_FIRST_SOURCE: u32 = 1;
const PLIC_SOURCE_COUNT: usize = 1023;
static PLIC_REGISTRY: ControllerIrqRegistry<PLIC_SOURCE_COUNT> =
    ControllerIrqRegistry::new(PLIC_FIRST_SOURCE);

pub(crate) fn setup_hybrid_forwarding(
    vm: &crate::AxVMRef,
    cpu_id: usize,
    generation: usize,
) -> AxVmResult {
    let owner = vm
        .id()
        .checked_add(1)
        .expect("VM ID must leave zero available as the unowned PLIC marker");
    let routes = resolve_hybrid_routes(vm)?;
    for route in &routes {
        PLIC_REGISTRY
            .bind_domain(route.host_irq().domain)
            .map_err(|domain| {
                AxVmError::interrupt(
                    "bind RISC-V Hybrid PLIC domain",
                    format_args!("registry is already bound to {domain:?}"),
                )
            })?;
    }
    let claims = PLIC_REGISTRY
        .claim_all(owner, generation, &routes)
        .map_err(|route| {
            AxVmError::resource_conflict(
                "RISC-V PLIC source",
                format_args!(
                    "source {} is already owned by another VM",
                    route.host_irq().hwirq.0
                ),
            )
        })?;
    let affinity = ax_hal::irq::IrqAffinity::Fixed(ax_hal::irq::CpuId(cpu_id));
    for route in routes {
        ax_hal::irq::set_affinity(route.host_irq(), affinity).map_err(|error| {
            AxVmError::interrupt("route RISC-V Hybrid PLIC source", format_args!("{error:?}"))
        })?;
    }
    claims.commit();
    Ok(())
}

pub(crate) fn unregister_hybrid_forwarding(vm: &crate::AxVMRef, generation: usize) {
    let routes = vm.with_config(|config| {
        config
            .pass_through_irqs()
            .iter()
            .filter_map(|&source| {
                PLIC_REGISTRY
                    .bound_irq(source)
                    .map(|irq| PhysicalIrqRoute::new(irq, source as usize))
            })
            .collect::<alloc::vec::Vec<_>>()
    });
    let Some(owner) = vm.id().checked_add(1) else {
        return;
    };
    PLIC_REGISTRY.release_generation(owner, generation, &routes);
}

pub(crate) fn hybrid_guest_irq(
    vm: &crate::AxVMRef,
    irq: ax_hal::irq::IrqId,
    generation: usize,
) -> Option<usize> {
    if vm.interrupt_mode() != VMInterruptMode::Hybrid {
        return None;
    }
    let owner = vm.id().checked_add(1)?;
    if !PLIC_REGISTRY.is_active_owner(irq, owner, generation) {
        return None;
    }
    vm.with_config(|config| {
        config
            .pass_through_irqs()
            .contains(&irq.hwirq.0)
            .then(|| PhysicalIrqRoute::new(irq, irq.hwirq.0 as usize))
            .map(PhysicalIrqRoute::guest_irq)
    })
}

fn resolve_hybrid_routes(vm: &crate::AxVMRef) -> AxVmResult<alloc::vec::Vec<PhysicalIrqRoute>> {
    vm.with_config(|config| config.pass_through_irqs().to_vec())
        .into_iter()
        .map(|source| {
            ax_hal::irq::resolve_external_irq(ax_hal::irq::HwIrq(source))
                .map(|irq| PhysicalIrqRoute::new(irq, source as usize))
                .map_err(|error| {
                    AxVmError::interrupt(
                        "resolve RISC-V Hybrid PLIC source",
                        format_args!("source {source}: {error:?}"),
                    )
                })
        })
        .collect()
}

struct RiscvPlicIrqSink {
    vplic: Arc<VPlicGlobal>,
}

impl IrqSink for RiscvPlicIrqSink {
    fn set_level(&self, line: IrqLineId, asserted: bool) -> IrqResult {
        let result = if asserted {
            self.vplic.set_pending(line.0)
        } else {
            self.vplic.clear_pending(line.0)
        };
        result.map_err(|error| IrqError::Backend {
            line,
            operation: "set vPLIC line level",
            detail: alloc::format!("{error}"),
        })
    }

    fn pulse(&self, line: IrqLineId) -> IrqResult {
        self.vplic
            .set_pending(line.0)
            .map_err(|error| IrqError::Backend {
                line,
                operation: "pulse vPLIC line",
                detail: alloc::format!("{error}"),
            })
    }
}

struct RiscvPlicFactory {
    base_gpa: usize,
    length: usize,
    contexts_num: usize,
    vplic: Arc<VPlicGlobal>,
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
        Ok(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(self.vplic.clone())).into())
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
) -> AxVmResult<InterruptFabric> {
    let mut vplic_configs = configs
        .iter()
        .filter(|config| config.emu_type == EmulatedDeviceType::PPPTGlobal);
    let Some(config) = vplic_configs.next() else {
        return Ok(InterruptFabric::new(mode));
    };
    if vplic_configs.next().is_some() {
        return ax_err!(
            AlreadyExists,
            "a VM can register only one virtual PLIC global controller"
        );
    }

    let contexts_num = validate_vplic_config(config)?;
    let vplic = Arc::new(
        VPlicGlobal::new(config.base_gpa.into(), Some(config.length), contexts_num)
            .map_err(AxVmError::invalid_config)?,
    );
    factories.register(Arc::new(RiscvPlicFactory {
        base_gpa: config.base_gpa,
        length: config.length,
        contexts_num,
        vplic: vplic.clone(),
    }))?;

    InterruptFabric::with_sink(mode, Arc::new(RiscvPlicIrqSink { vplic }))
}
