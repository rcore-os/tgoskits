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

#![cfg(feature = "vgicv3")]

use std::time::Duration;

use arm_vgic::{
    host::ArmVgicHostIf,
    v3::vgicr::{DEFAULT_SIZE_PER_GICR, GpptRedistributorFactory},
};
use ax_memory_addr::{PhysAddr, VirtAddr};
use axdevice_base::{
    DeviceBundle, DeviceFactory, DeviceFactoryContext, DeviceFactoryError, DeviceFactoryResult,
    IrqLine,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType, InterruptTriggerMode};

struct UnreachableArmVgicHost;

#[ax_crate_interface::impl_interface]
impl ArmVgicHostIf for UnreachableArmVgicHost {
    fn alloc_contiguous_frames(_frame_count: usize, _frame_align: usize) -> Option<PhysAddr> {
        unexpected_host_access()
    }

    fn dealloc_contiguous_frames(_start_paddr: PhysAddr, _frame_count: usize) {
        unexpected_host_access()
    }

    fn phys_to_virt(_paddr: PhysAddr) -> VirtAddr {
        unexpected_host_access()
    }

    fn host_cpu_num() -> usize {
        unexpected_host_access()
    }

    fn current_vcpu_id() -> usize {
        unexpected_host_access()
    }

    fn current_time_nanos() -> u64 {
        unexpected_host_access()
    }

    fn register_timer(_deadline: Duration, _callback: Box<dyn FnOnce(Duration) + Send + 'static>) {
        unexpected_host_access()
    }

    fn read_vgicd_iidr() -> u32 {
        unexpected_host_access()
    }

    fn read_vgicd_typer() -> u32 {
        unexpected_host_access()
    }

    fn get_host_gicd_base() -> PhysAddr {
        unexpected_host_access()
    }

    fn get_host_gicr_base() -> PhysAddr {
        unexpected_host_access()
    }

    fn hardware_inject_virtual_interrupt(_vector: u8) {
        unexpected_host_access()
    }
}

fn unexpected_host_access() -> ! {
    panic!("invalid factory configurations must not access the VGIC host")
}

struct UnusedFactoryContext;

impl DeviceFactoryContext for UnusedFactoryContext {
    fn resolve_irq(
        &self,
        _line: usize,
        _trigger: InterruptTriggerMode,
    ) -> DeviceFactoryResult<IrqLine> {
        unreachable!("GPPT redistributor factory does not resolve IRQ lines")
    }
}

#[test]
fn rejects_overflowing_redistributor_window() {
    let config = redistributor_config(
        usize::MAX - 2 * DEFAULT_SIZE_PER_GICR + 1,
        DEFAULT_SIZE_PER_GICR,
        2,
        DEFAULT_SIZE_PER_GICR,
    );

    assert_invalid_config(
        GpptRedistributorFactory.build(&config, &UnusedFactoryContext),
        "redistributor address range overflows",
    );
}

#[test]
fn rejects_empty_redistributor_window() {
    let config = redistributor_config(0x080a_0000, 0, 1, DEFAULT_SIZE_PER_GICR);

    assert_invalid_config(
        GpptRedistributorFactory.build(&config, &UnusedFactoryContext),
        "redistributor length must be non-zero",
    );
}

#[test]
fn rejects_stride_smaller_than_redistributor_length() {
    let config = redistributor_config(
        0x080a_0000,
        DEFAULT_SIZE_PER_GICR,
        2,
        DEFAULT_SIZE_PER_GICR - 1,
    );

    assert_invalid_config(
        GpptRedistributorFactory.build(&config, &UnusedFactoryContext),
        "redistributor stride is smaller than its length",
    );
}

fn redistributor_config(
    base_gpa: usize,
    length: usize,
    cpu_count: usize,
    stride: usize,
) -> EmulatedDeviceConfig {
    EmulatedDeviceConfig {
        name: "gppt-gicr".into(),
        base_gpa,
        length,
        irq_id: 0,
        emu_type: EmulatedDeviceType::GPPTRedistributor,
        cfg_list: vec![cpu_count, stride, 0],
    }
}

fn assert_invalid_config(result: DeviceFactoryResult<DeviceBundle>, expected_detail: &str) {
    match result {
        Err(DeviceFactoryError::InvalidConfig { operation, detail }) => {
            assert_eq!(operation, "build GPPT redistributor");
            assert!(
                detail.contains(expected_detail),
                "unexpected invalid-config detail: {detail}"
            );
        }
        Err(other) => panic!("expected invalid config, got {other:?}"),
        Ok(_) => panic!("expected invalid config, factory accepted the configuration"),
    }
}
