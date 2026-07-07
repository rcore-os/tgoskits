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

use ax_errno::{AxError, AxResult};
use axaddrspace::GuestPhysAddr;
use axvisor_api::control as api_control;
use axvm::AxVMRef;

use super::super::{KVM_MP_STATE_RUNNABLE, set_vcpu_file_mp_state_by_id};
use crate::vmm::{
    interrupt::{VcpuInterruptTarget, VirtualInterrupt, deliver_targeted_interrupt},
    vcpus::{guest_cpu_id_for_vcpu, guest_cpu_id_to_vcpu_id},
};

pub(super) fn handle_cpu_up(
    control_file: api_control::ControlFileId,
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    target_cpu: usize,
    entry_point: GuestPhysAddr,
    arg: u64,
) -> AxResult {
    let target_vcpu_id = guest_cpu_id_to_vcpu_id(vm, target_cpu).ok_or(AxError::InvalidInput)?;
    let target_vcpu = vm.vcpu(target_vcpu_id).ok_or(AxError::InvalidInput)?;

    target_vcpu.set_entry(entry_point)?;
    target_vcpu.set_gpr(
        riscv_vcpu::GprIndex::A0 as usize,
        guest_cpu_id_for_vcpu(vm, target_vcpu_id),
    );
    target_vcpu.set_gpr(riscv_vcpu::GprIndex::A1 as usize, arg as usize);

    set_vcpu_file_mp_state_by_id(control_file, target_vcpu_id, KVM_MP_STATE_RUNNABLE)?;

    vcpu.set_return_value(0);
    vcpu.set_gpr(riscv_vcpu::GprIndex::A1 as usize, 0);

    Ok(())
}

pub(super) fn handle_send_ipi(
    vm: &AxVMRef,
    current_vcpu_id: usize,
    target_cpu: usize,
    target_cpu_aux: usize,
    send_to_all: bool,
    send_to_self: bool,
    vector: usize,
) -> AxResult {
    let target = if send_to_all {
        VcpuInterruptTarget::All {
            current_vcpu_id,
            include_current: send_to_self,
        }
    } else if send_to_self {
        VcpuInterruptTarget::Vcpu(current_vcpu_id)
    } else {
        VcpuInterruptTarget::GuestCpuMask {
            mask: target_cpu,
            base: target_cpu_aux,
        }
    };
    deliver_targeted_interrupt(vm, target, VirtualInterrupt::edge(vector))
}
