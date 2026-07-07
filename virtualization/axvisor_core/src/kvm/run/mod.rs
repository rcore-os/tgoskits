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

use ax_errno::{AxErrorKind, AxResult, ax_err};
use axvcpu::AxVCpuExitReason;
use axvisor_api::control as api_control;

mod exit;
#[cfg(target_arch = "riscv64")]
mod riscv;
mod x86;

use exit::{complete_io_read, complete_mmio_read, kvm_exit_reason, prepare_userspace_exit};
#[cfg(target_arch = "riscv64")]
use riscv::{handle_cpu_up, handle_send_ipi};
#[cfg(target_arch = "x86_64")]
use x86::{
    handle_cpu_up, handle_in_kernel_device_exit, handle_kvm_msr_write, inject_in_kernel_device_irqs,
};
use x86::{update_vcpu_run_interrupt_state, vcpu_run_irq_window_open};

use super::{CONTROL_FILES, ControlFileState, take_control_vcpu_interrupts};
use crate::kvm::{
    abi::raw as abi,
    eventfd::signal_matching_ioeventfd,
    util::{read_vcpu_run_u8, write_vcpu_run_u32},
};

pub(in crate::kvm) fn run_vcpu_file(control_file: api_control::ControlFileId) -> AxResult<isize> {
    let (
        vm,
        vcpu_id,
        vcpu,
        mp_state,
        pending_mmio_read,
        pending_io_read,
        irqchip_created,
        pit2_created,
    ) = {
        let mut control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        let Some(ControlFileState::Vm(vm)) = control_files.get(&vcpu.vm_file) else {
            return ax_err!(NotFound);
        };
        let irqchip_created = vm.irqchip_created;
        let pit2_created = vm.pit2_created;
        let vm = vm.vm.clone();
        let vcpu_id = vcpu.vcpu_id as usize;
        let mp_state = vcpu.mp_state;
        let pending_mmio_read = vcpu.pending_mmio_read;
        let pending_io_read = vcpu.pending_io_read;
        let Some(ControlFileState::Vcpu(vcpu_file_state)) = control_files.get_mut(&control_file)
        else {
            return ax_err!(NotFound);
        };
        vcpu_file_state.pending_mmio_read = None;
        vcpu_file_state.pending_io_read = None;
        let Some(vcpu) = vm.vcpu(vcpu_id) else {
            return ax_err!(NotFound);
        };
        (
            vm,
            vcpu_id,
            vcpu,
            mp_state,
            pending_mmio_read,
            pending_io_read,
            irqchip_created,
            pit2_created,
        )
    };
    #[cfg(not(target_arch = "x86_64"))]
    let _ = (irqchip_created, pit2_created);

    let _context_guard = crate::context::bind_current_vcpu_context(vm.id(), vcpu_id);

    if mp_state == abi::KVM_MP_STATE_STOPPED {
        wait_until_vcpu_runnable(control_file)?;
    }

    if !vm.running() {
        vm.boot()?;
    }

    if let Some(pending) = pending_mmio_read {
        complete_mmio_read(control_file, &vcpu, pending)?;
    }
    if let Some(pending) = pending_io_read {
        complete_io_read(control_file, &vcpu, pending)?;
    }

    update_vcpu_run_interrupt_state(control_file, &vcpu)?;
    if read_vcpu_run_u8(control_file, abi::KVM_RUN_IMMEDIATE_EXIT_OFFSET)? != 0 {
        return Err(AxErrorKind::Interrupted.into());
    }

    let exit_reason = loop {
        for vector in take_control_vcpu_interrupts(control_file) {
            vcpu.inject_interrupt(vector)?;
        }
        update_vcpu_run_interrupt_state(control_file, &vcpu)?;
        if read_vcpu_run_u8(control_file, abi::KVM_RUN_IMMEDIATE_EXIT_OFFSET)? != 0 {
            return Err(AxErrorKind::Interrupted.into());
        }
        if vcpu_run_irq_window_open(control_file)? {
            break abi::KVM_EXIT_IRQ_WINDOW_OPEN;
        }

        let exit_reason = match vm.run_vcpu_raw(vcpu_id) {
            Ok(exit_reason) => exit_reason,
            Err(err) => {
                warn!("KVM_RUN vCPU error: {:?}", err);
                break abi::KVM_EXIT_INTERNAL_ERROR;
            }
        };

        #[cfg(target_arch = "x86_64")]
        if handle_in_kernel_device_exit(&vm, &vcpu, irqchip_created, pit2_created, &exit_reason)? {
            continue;
        }

        match exit_reason {
            AxVCpuExitReason::Nothing
            | AxVCpuExitReason::PreemptionTimer
            | AxVCpuExitReason::ExternalInterrupt { .. }
            | AxVCpuExitReason::InterruptEnd { .. } => {
                crate::vmm::vcpus::handle_internal_exit(&vm, &vcpu, &exit_reason);
                axvisor_api::task::yield_now();
            }
            #[cfg(target_arch = "x86_64")]
            AxVCpuExitReason::Halt if irqchip_created => wait_for_halted_vcpu_wakeup(
                control_file,
                &vm,
                &vcpu,
                irqchip_created,
                pit2_created,
            )?,
            AxVCpuExitReason::MmioWrite { addr, width, data }
                if signal_matching_ioeventfd(
                    control_file,
                    addr.as_usize() as u64,
                    width,
                    data,
                    false,
                )? => {}
            AxVCpuExitReason::IoWrite { port, width, data }
                if signal_matching_ioeventfd(
                    control_file,
                    port.number() as u64,
                    width,
                    data,
                    true,
                )? => {}
            #[cfg(target_arch = "x86_64")]
            AxVCpuExitReason::SysRegWrite { addr, value }
                if handle_kvm_msr_write(&vm, addr.addr(), value)? =>
            {
                axvisor_api::task::yield_now();
            }
            #[cfg(target_arch = "x86_64")]
            AxVCpuExitReason::Hypercall {
                nr: abi::KVM_HC_CLOCK_PAIRING,
                ..
            } => {
                vcpu.set_gpr(abi::X86_RAX_REG_INDEX, (-abi::KVM_ENOSYS) as usize);
                axvisor_api::task::yield_now();
            }
            #[cfg(target_arch = "x86_64")]
            AxVCpuExitReason::CpuUp {
                target_cpu,
                entry_point,
                ..
            } => {
                handle_cpu_up(control_file, &vm, target_cpu as usize, entry_point)?;
                axvisor_api::task::yield_now();
            }
            #[cfg(target_arch = "riscv64")]
            AxVCpuExitReason::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => {
                handle_cpu_up(
                    control_file,
                    &vm,
                    &vcpu,
                    target_cpu as usize,
                    entry_point,
                    arg,
                )?;
                axvisor_api::task::yield_now();
            }
            #[cfg(target_arch = "riscv64")]
            AxVCpuExitReason::SendIPI {
                target_cpu,
                target_cpu_aux,
                send_to_all,
                send_to_self,
                vector,
            } => {
                handle_send_ipi(
                    &vm,
                    vcpu_id,
                    target_cpu as usize,
                    target_cpu_aux as usize,
                    send_to_all,
                    send_to_self,
                    vector as usize,
                )?;
                axvisor_api::task::yield_now();
            }
            exit_reason => {
                let kvm_reason = kvm_exit_reason(&exit_reason);
                prepare_userspace_exit(control_file, &exit_reason)?;
                break kvm_reason;
            }
        }
    };
    write_vcpu_run_u32(control_file, abi::KVM_RUN_EXIT_REASON_OFFSET, exit_reason)?;
    Ok(0)
}

fn wait_until_vcpu_runnable(control_file: api_control::ControlFileId) -> AxResult {
    loop {
        if read_vcpu_run_u8(control_file, abi::KVM_RUN_IMMEDIATE_EXIT_OFFSET)? != 0 {
            return Err(AxErrorKind::Interrupted.into());
        }
        if current_vcpu_mp_state(control_file)? != abi::KVM_MP_STATE_STOPPED {
            return Ok(());
        }
        axvisor_api::task::yield_now();
    }
}

#[cfg(target_arch = "x86_64")]
fn wait_for_halted_vcpu_wakeup(
    control_file: api_control::ControlFileId,
    vm: &axvm::AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    irqchip_created: bool,
    pit2_created: bool,
) -> AxResult {
    set_current_vcpu_halted(control_file, true)?;
    loop {
        if read_vcpu_run_u8(control_file, abi::KVM_RUN_IMMEDIATE_EXIT_OFFSET)? != 0 {
            set_current_vcpu_halted(control_file, false)?;
            return Err(AxErrorKind::Interrupted.into());
        }
        if current_vcpu_has_pending_interrupt(control_file)? {
            set_current_vcpu_halted(control_file, false)?;
            return Ok(());
        }
        if !current_vcpu_halted(control_file)? {
            return Ok(());
        }
        if inject_in_kernel_device_irqs(vm, vcpu, irqchip_created, pit2_created) {
            set_current_vcpu_halted(control_file, false)?;
            return Ok(());
        }
        axvisor_api::task::yield_now();
    }
}

fn current_vcpu_mp_state(control_file: api_control::ControlFileId) -> AxResult<u32> {
    let control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
        return ax_err!(NotFound);
    };
    Ok(vcpu.mp_state)
}

#[cfg(target_arch = "x86_64")]
fn set_current_vcpu_halted(control_file: api_control::ControlFileId, halted: bool) -> AxResult {
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vcpu.halted = halted;
    Ok(())
}

#[cfg(target_arch = "x86_64")]
fn current_vcpu_halted(control_file: api_control::ControlFileId) -> AxResult<bool> {
    let control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
        return ax_err!(NotFound);
    };
    Ok(vcpu.halted)
}

#[cfg(target_arch = "x86_64")]
fn current_vcpu_has_pending_interrupt(control_file: api_control::ControlFileId) -> AxResult<bool> {
    let control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
        return ax_err!(NotFound);
    };
    Ok(!vcpu.pending_interrupts.is_empty())
}
