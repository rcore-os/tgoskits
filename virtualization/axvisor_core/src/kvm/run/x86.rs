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

#[cfg(target_arch = "x86_64")]
use ax_errno::AxError;
use ax_errno::AxResult;
#[cfg(target_arch = "x86_64")]
use axaddrspace::GuestPhysAddr;
#[cfg(target_arch = "x86_64")]
use axdevice_base::EmuDeviceType;
#[cfg(target_arch = "x86_64")]
use axvcpu::AxVCpuExitReason;
use axvisor_api::control as api_control;
#[cfg(target_arch = "x86_64")]
use axvm::AxVMRef;

#[cfg(target_arch = "x86_64")]
use crate::kvm::abi::raw as abi;
#[cfg(target_arch = "x86_64")]
use crate::kvm::{
    cpuid::default_tsc_khz,
    set_vcpu_file_mp_state_by_id,
    state::{PvClockVcpuTimeInfo, PvClockWallClock},
    util::{
        access_width_mask, read_vcpu_run_u8, sign_extend_value, write_vcpu_run_u8,
        write_vcpu_run_u16,
    },
    vcpu_file_mp_state_by_id,
};

#[cfg(target_arch = "x86_64")]
pub(super) fn handle_kvm_msr_write(vm: &AxVMRef, msr: usize, value: u64) -> AxResult<bool> {
    match msr {
        abi::MSR_KVM_WALL_CLOCK_NEW => {
            write_kvm_wall_clock(vm, GuestPhysAddr::from(value as usize))?;
            Ok(true)
        }
        abi::MSR_KVM_SYSTEM_TIME_NEW => {
            if value & abi::KVM_SYSTEM_TIME_ENABLE != 0 {
                let gpa = GuestPhysAddr::from((value & !abi::KVM_SYSTEM_TIME_ENABLE) as usize);
                write_kvm_system_time(vm, gpa)?;
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(target_arch = "x86_64")]
pub(super) fn handle_cpu_up(
    control_file: api_control::ControlFileId,
    vm: &AxVMRef,
    target_cpu: usize,
    entry_point: GuestPhysAddr,
) -> AxResult {
    if vcpu_file_mp_state_by_id(control_file, target_cpu)? != abi::KVM_MP_STATE_STOPPED {
        return Ok(());
    }

    let target_vcpu = vm.vcpu(target_cpu).ok_or(AxError::InvalidInput)?;
    target_vcpu.set_entry(entry_point)?;
    set_vcpu_file_mp_state_by_id(control_file, target_cpu, abi::KVM_MP_STATE_RUNNABLE)
}

#[cfg(target_arch = "x86_64")]
pub(super) fn handle_in_kernel_device_exit(
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    irqchip_created: bool,
    pit2_created: bool,
    exit_reason: &AxVCpuExitReason,
) -> AxResult<bool> {
    match exit_reason {
        AxVCpuExitReason::MmioRead {
            addr,
            width,
            reg,
            reg_width,
            signed_ext,
        } if irqchip_created && is_x86_ioapic_mmio(vm, *addr) => {
            let raw = vm.get_devices().handle_mmio_read(*addr, *width)?;
            let masked = raw & access_width_mask(*width);
            let val = if *signed_ext {
                sign_extend_value(masked, *width)
            } else {
                masked & access_width_mask(*reg_width)
            };
            vcpu.set_gpr(*reg, val);
            Ok(true)
        }
        AxVCpuExitReason::MmioWrite { addr, width, data }
            if irqchip_created && is_x86_ioapic_mmio(vm, *addr) =>
        {
            vm.get_devices()
                .handle_mmio_write(*addr, *width, *data as usize)?;
            Ok(true)
        }
        AxVCpuExitReason::IoRead { port, width } if pit2_created && is_x86_pit_port(*port) => {
            let val = vm.get_devices().handle_port_read(*port, *width)?;
            vcpu.set_gpr(0, val);
            Ok(true)
        }
        AxVCpuExitReason::IoWrite { port, width, data }
            if pit2_created && is_x86_pit_port(*port) =>
        {
            vm.get_devices()
                .handle_port_write(*port, *width, *data as usize)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(target_arch = "x86_64")]
pub(super) fn inject_in_kernel_device_irqs(
    vm: &AxVMRef,
    vcpu: &axvm::AxVCpuRef,
    irqchip_created: bool,
    pit2_created: bool,
) -> bool {
    if !irqchip_created {
        return false;
    }
    let mut injected = false;
    if pit2_created {
        injected |= crate::vmm::devices::x86::inject_due_pit_irq0(vm, vcpu);
    }
    injected |= crate::vmm::devices::x86::inject_pending_serial_irq(vm, vcpu);
    injected
}

#[cfg(target_arch = "x86_64")]
pub(super) fn update_vcpu_run_interrupt_state(
    control_file: api_control::ControlFileId,
    vcpu: &axvm::AxVCpuRef,
) -> AxResult {
    let mut regs = [0u8; abi::KVM_X86_REGS_SIZE as usize];
    let if_flag = if vcpu.get_kvm_regs(&mut regs).is_ok() {
        let mut rflags_bytes = [0u8; 8];
        rflags_bytes.copy_from_slice(
            &regs[abi::KVM_X86_REGS_RFLAGS_OFFSET..abi::KVM_X86_REGS_RFLAGS_OFFSET + 8],
        );
        u8::from(u64::from_ne_bytes(rflags_bytes) & abi::X86_RFLAGS_IF != 0)
    } else {
        1
    };

    write_vcpu_run_u8(
        control_file,
        abi::KVM_RUN_READY_FOR_INTERRUPT_INJECTION_OFFSET,
        if_flag,
    )?;
    write_vcpu_run_u8(control_file, abi::KVM_RUN_IF_FLAG_OFFSET, if_flag)?;
    write_vcpu_run_u16(control_file, abi::KVM_RUN_FLAGS_OFFSET, 0)?;
    Ok(())
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn update_vcpu_run_interrupt_state(
    _control_file: api_control::ControlFileId,
    _vcpu: &axvm::AxVCpuRef,
) -> AxResult {
    Ok(())
}

#[cfg(target_arch = "x86_64")]
pub(super) fn vcpu_run_irq_window_open(control_file: api_control::ControlFileId) -> AxResult<bool> {
    Ok(
        read_vcpu_run_u8(control_file, abi::KVM_RUN_REQUEST_INTERRUPT_WINDOW_OFFSET)? != 0
            && read_vcpu_run_u8(
                control_file,
                abi::KVM_RUN_READY_FOR_INTERRUPT_INJECTION_OFFSET,
            )? != 0,
    )
}

#[cfg(not(target_arch = "x86_64"))]
pub(super) fn vcpu_run_irq_window_open(
    _control_file: api_control::ControlFileId,
) -> AxResult<bool> {
    Ok(false)
}

#[cfg(target_arch = "x86_64")]
fn write_kvm_wall_clock(vm: &AxVMRef, gpa: GuestPhysAddr) -> AxResult {
    let now_ns = axvisor_api::time::current_time_nanos() as u64;
    let wall_clock = PvClockWallClock {
        version: 2,
        sec: (now_ns / 1_000_000_000) as u32,
        nsec: (now_ns % 1_000_000_000) as u32,
    };
    vm.write_to_guest_of(gpa, &wall_clock)
}

#[cfg(target_arch = "x86_64")]
fn write_kvm_system_time(vm: &AxVMRef, gpa: GuestPhysAddr) -> AxResult {
    let tsc_khz = default_tsc_khz().max(1);
    let info = PvClockVcpuTimeInfo {
        version: 2,
        pad0: 0,
        tsc_timestamp: unsafe { core::arch::x86_64::_rdtsc() },
        system_time: axvisor_api::time::current_time_nanos() as u64,
        tsc_to_system_mul: tsc_to_system_mul(tsc_khz),
        tsc_shift: 0,
        flags: abi::PVCLOCK_TSC_STABLE_BIT,
        pad: [0; 2],
    };
    vm.write_to_guest_of(gpa, &info)
}

#[cfg(target_arch = "x86_64")]
fn tsc_to_system_mul(tsc_khz: u32) -> u32 {
    (((1_000_000u128) << 32) / u128::from(tsc_khz)).min(u128::from(u32::MAX)) as u32
}

#[cfg(target_arch = "x86_64")]
fn is_x86_ioapic_mmio(vm: &AxVMRef, addr: GuestPhysAddr) -> bool {
    vm.get_devices()
        .find_mmio_dev(addr)
        .is_some_and(|dev| dev.emu_type() == EmuDeviceType::X86IoApic)
}

#[cfg(target_arch = "x86_64")]
fn is_x86_pit_port(port: axaddrspace::device::Port) -> bool {
    matches!(port.number(), 0x40..=0x43 | 0x61)
}
