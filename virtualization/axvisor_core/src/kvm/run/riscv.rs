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

pub(super) fn handle_cpu_up(
    control_file: api_control::ControlFileId,
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    target_cpu: usize,
    entry_point: GuestPhysAddr,
    arg: u64,
) -> AxResult {
    let target_vcpu = vm.vcpu(target_cpu).ok_or(AxError::InvalidInput)?;

    target_vcpu.set_entry(entry_point)?;
    target_vcpu.set_gpr(riscv_vcpu::GprIndex::A0 as usize, target_cpu);
    target_vcpu.set_gpr(riscv_vcpu::GprIndex::A1 as usize, arg as usize);

    set_vcpu_file_mp_state_by_id(control_file, target_cpu, KVM_MP_STATE_RUNNABLE)?;

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
    if !send_to_all && !send_to_self {
        return inject_riscv_ipi_mask(vm, target_cpu, target_cpu_aux, vector);
    }

    if send_to_all {
        for target_vcpu_id in 0..vm.vcpu_num() {
            if target_vcpu_id != current_vcpu_id || send_to_self {
                vm.vcpu(target_vcpu_id)
                    .ok_or(AxError::InvalidInput)?
                    .inject_interrupt(vector)?;
            }
        }
        return Ok(());
    }

    let target_vcpu_id = if send_to_self {
        current_vcpu_id
    } else {
        target_cpu
    };
    vm.vcpu(target_vcpu_id)
        .ok_or(AxError::InvalidInput)?
        .inject_interrupt(vector)
}

fn inject_riscv_ipi_mask(
    vm: &AxVMRef,
    hart_mask: usize,
    hart_mask_base: usize,
    vector: usize,
) -> AxResult {
    for target_vcpu_id in 0..vm.vcpu_num() {
        let selected = if hart_mask_base == usize::MAX {
            true
        } else {
            target_vcpu_id
                .checked_sub(hart_mask_base)
                .filter(|bit| *bit < usize::BITS as usize)
                .is_some_and(|bit| (hart_mask & (1usize << bit)) != 0)
        };

        if selected {
            vm.vcpu(target_vcpu_id)
                .ok_or(AxError::InvalidInput)?
                .inject_interrupt(vector)?;
        }
    }

    Ok(())
}
