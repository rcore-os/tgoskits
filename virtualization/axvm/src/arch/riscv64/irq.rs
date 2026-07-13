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

use ax_errno::{AxResult, ax_err};
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceRegistration,
    MmioDeviceAdapter,
};
use axdevice_base::{IrqLineId, IrqSink};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, VMInterruptMode};
use riscv_vplic::{
    PLIC_CONTEXT_CLAIM_COMPLETE_OFFSET, PLIC_CONTEXT_CTRL_OFFSET, PLIC_CONTEXT_STRIDE, VPlicGlobal,
};

use crate::irq::InterruptFabric;

struct RiscvPlicIrqSink {
    vplic: Arc<VPlicGlobal>,
}

impl IrqSink for RiscvPlicIrqSink {
    fn set_level(&self, line: IrqLineId, asserted: bool) -> AxResult {
        if asserted {
            self.vplic.set_pending(line.0)
        } else {
            self.vplic.clear_pending(line.0)
        }
    }

    fn pulse(&self, line: IrqLineId) -> AxResult {
        self.vplic.set_pending(line.0)
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
    ) -> AxResult<DeviceBundle> {
        if config.base_gpa != self.base_gpa
            || config.length != self.length
            || config.cfg_list.as_slice() != [self.contexts_num]
        {
            return ax_err!(
                InvalidInput,
                format_args!(
                    "virtual PLIC configuration changed while building device '{}'",
                    config.name
                )
            );
        }
        Ok(DeviceRegistration::Device(MmioDeviceAdapter::from_arc(self.vplic.clone())).into())
    }
}

fn validate_vplic_config(config: &EmulatedDeviceConfig) -> AxResult<usize> {
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
        .ok_or(ax_errno::AxError::InvalidInput)?;
    let region_end = config
        .base_gpa
        .checked_add(config.length)
        .ok_or(ax_errno::AxError::InvalidInput)?;
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
) -> AxResult<InterruptFabric> {
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
    let vplic = Arc::new(VPlicGlobal::new(
        config.base_gpa.into(),
        Some(config.length),
        contexts_num,
    ));
    factories.register(Arc::new(RiscvPlicFactory {
        base_gpa: config.base_gpa,
        length: config.length,
        contexts_num,
        vplic: vplic.clone(),
    }))?;

    InterruptFabric::with_sink(mode, Arc::new(RiscvPlicIrqSink { vplic }))
}
