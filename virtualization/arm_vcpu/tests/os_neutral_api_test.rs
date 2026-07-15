// Copyright 2026 The Axvisor Team
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

#![cfg(target_arch = "aarch64")]

use core::mem::size_of;

use arm_vcpu::{
    ARM_VCPU_HOST_SP_EL0_OFFSET, ARM_VCPU_HOST_STACK_TOP_OFFSET, ARM_VCPU_TRAP_FRAME_SIZE,
    Aarch64PerCpu, Aarch64VCpu, ArmAccessWidth, ArmGuestPhysAddr, ArmHostOps,
    ArmNestedPagingConfig, ArmPerCpu, ArmSysRegAddr, ArmVcpu, ArmVcpuError, ArmVcpuResult,
    ArmVmExit, TrapFrame,
};

struct DummyHost;

impl ArmHostOps for DummyHost {
    fn handle_current_host_irq() {}
}

#[test]
fn vcpu_and_percpu_types_are_host_generic_without_axvm_traits() {
    let _vcpu: Option<ArmVcpu<DummyHost>> = None;
    let _compat_vcpu: Option<Aarch64VCpu<DummyHost>> = None;
    let _percpu: Option<ArmPerCpu> = None;
    let _compat_percpu: Option<Aarch64PerCpu> = None;

    assert_eq!(ARM_VCPU_TRAP_FRAME_SIZE, 34 * size_of::<u64>());
    assert_eq!(size_of::<TrapFrame>(), ARM_VCPU_TRAP_FRAME_SIZE);
    assert_eq!(ARM_VCPU_HOST_STACK_TOP_OFFSET, ARM_VCPU_TRAP_FRAME_SIZE);
    assert_eq!(
        ARM_VCPU_HOST_SP_EL0_OFFSET,
        ARM_VCPU_HOST_STACK_TOP_OFFSET + size_of::<u64>()
    );
}

#[test]
fn nested_paging_config_uses_os_neutral_integer_values() {
    let config = ArmNestedPagingConfig::new(0x1000, 3, 39, 48);

    assert_eq!(config.root_paddr, 0x1000);
    assert_eq!(config.levels, 3);
    assert_eq!(config.gpa_bits, 39);
    assert_eq!(config.mode, 48);
}

#[test]
fn vm_exit_types_are_defined_by_arm_vcpu_core() {
    let exit = ArmVmExit::MmioRead {
        addr: ArmGuestPhysAddr::from_usize(0x2000),
        width: ArmAccessWidth::Dword,
        reg: 3,
        reg_width: ArmAccessWidth::Qword,
        signed_ext: false,
    };

    match exit {
        ArmVmExit::MmioRead {
            addr, width, reg, ..
        } => {
            assert_eq!(addr.as_usize(), 0x2000);
            assert_eq!(width.size(), 4);
            assert_eq!(reg, 3);
        }
        other => panic!("unexpected exit: {other:?}"),
    }

    let exit = ArmVmExit::SysRegRead {
        addr: ArmSysRegAddr::new(0x3a_3016),
        reg: 1,
    };
    assert!(matches!(
        exit,
        ArmVmExit::SysRegRead {
            addr,
            reg: 1,
        } if addr.addr() == 0x3a_3016
    ));

    let sgi1r = 0x12_0003_45_0007u64;
    assert!(matches!(
        ArmVmExit::SendIPI { value: sgi1r },
        ArmVmExit::SendIPI { value } if value == sgi1r
    ));
    assert!(matches!(
        ArmVmExit::ExternalInterrupt,
        ArmVmExit::ExternalInterrupt
    ));
}

#[test]
fn host_ops_can_report_typed_errors() {
    assert_eq!(Err(ArmVcpuError::Unsupported), unsupported_host_call());
}

fn unsupported_host_call() -> ArmVcpuResult {
    Err(ArmVcpuError::Unsupported)
}
