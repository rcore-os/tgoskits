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

use ax_errno::{AxResult, ax_err};
use axvcpu::AxVCpuExitReason;
use axvisor_api::control as api_control;

use super::super::{CONTROL_FILES, ControlFileState};
use crate::kvm::{
    abi::raw as abi,
    state::{PendingIoRead, PendingMmioRead},
    util::{
        access_width_bytes, access_width_mask, control_file_mmap_area, sign_extend_value,
        write_vcpu_run_u8, write_vcpu_run_u16, write_vcpu_run_u32, write_vcpu_run_u64,
    },
};

pub(super) fn kvm_exit_reason(exit_reason: &AxVCpuExitReason) -> u32 {
    match exit_reason {
        AxVCpuExitReason::Halt => abi::KVM_EXIT_HLT,
        AxVCpuExitReason::IoRead { .. } | AxVCpuExitReason::IoWrite { .. } => abi::KVM_EXIT_IO,
        AxVCpuExitReason::MmioRead { .. } | AxVCpuExitReason::MmioWrite { .. } => {
            abi::KVM_EXIT_MMIO
        }
        AxVCpuExitReason::NestedPageFault { .. } => abi::KVM_EXIT_MEMORY_FAULT,
        AxVCpuExitReason::SystemDown => abi::KVM_EXIT_SHUTDOWN,
        AxVCpuExitReason::FailEntry { .. } => abi::KVM_EXIT_FAIL_ENTRY,
        AxVCpuExitReason::ExternalInterrupt { .. } | AxVCpuExitReason::PreemptionTimer => {
            abi::KVM_EXIT_INTR
        }
        _ => abi::KVM_EXIT_UNKNOWN,
    }
}

pub(super) fn prepare_userspace_exit(
    control_file: api_control::ControlFileId,
    exit_reason: &AxVCpuExitReason,
) -> AxResult {
    match exit_reason {
        AxVCpuExitReason::MmioRead {
            addr,
            width,
            reg,
            reg_width,
            signed_ext,
        } => {
            write_vcpu_run_u64(
                control_file,
                abi::KVM_RUN_MMIO_PHYS_ADDR_OFFSET,
                addr.as_usize() as u64,
            )?;
            write_vcpu_run_u32(
                control_file,
                abi::KVM_RUN_MMIO_LEN_OFFSET,
                access_width_bytes(*width),
            )?;
            write_vcpu_run_u8(control_file, abi::KVM_RUN_MMIO_IS_WRITE_OFFSET, 0)?;

            let mut control_files = CONTROL_FILES.lock();
            let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
                return ax_err!(NotFound);
            };
            vcpu.pending_mmio_read = Some(PendingMmioRead {
                reg: *reg,
                width: *width,
                reg_width: *reg_width,
                signed_ext: *signed_ext,
            });
        }
        AxVCpuExitReason::MmioWrite { addr, width, data } => {
            let mmap_area = control_file_mmap_area(control_file)?;
            write_vcpu_run_u64(
                control_file,
                abi::KVM_RUN_MMIO_PHYS_ADDR_OFFSET,
                addr.as_usize() as u64,
            )?;
            api_control::write_mmap_area(
                mmap_area,
                abi::KVM_RUN_MMIO_DATA_OFFSET,
                &data.to_ne_bytes(),
            )?;
            write_vcpu_run_u32(
                control_file,
                abi::KVM_RUN_MMIO_LEN_OFFSET,
                access_width_bytes(*width),
            )?;
            write_vcpu_run_u8(control_file, abi::KVM_RUN_MMIO_IS_WRITE_OFFSET, 1)?;
        }
        AxVCpuExitReason::IoRead { port, width } => {
            write_vcpu_run_u8(
                control_file,
                abi::KVM_RUN_IO_DIRECTION_OFFSET,
                abi::KVM_EXIT_IO_IN,
            )?;
            write_vcpu_run_u8(
                control_file,
                abi::KVM_RUN_IO_SIZE_OFFSET,
                access_width_bytes(*width) as u8,
            )?;
            write_vcpu_run_u16(control_file, abi::KVM_RUN_IO_PORT_OFFSET, port.number())?;
            write_vcpu_run_u32(control_file, abi::KVM_RUN_IO_COUNT_OFFSET, 1)?;
            write_vcpu_run_u64(
                control_file,
                abi::KVM_RUN_IO_DATA_OFFSET_OFFSET,
                abi::KVM_RUN_IO_DATA_OFFSET as u64,
            )?;

            let mut control_files = CONTROL_FILES.lock();
            let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
                return ax_err!(NotFound);
            };
            vcpu.pending_io_read = Some(PendingIoRead { width: *width });
        }
        AxVCpuExitReason::IoWrite { port, width, data } => {
            let mmap_area = control_file_mmap_area(control_file)?;
            write_vcpu_run_u8(
                control_file,
                abi::KVM_RUN_IO_DIRECTION_OFFSET,
                abi::KVM_EXIT_IO_OUT,
            )?;
            write_vcpu_run_u8(
                control_file,
                abi::KVM_RUN_IO_SIZE_OFFSET,
                access_width_bytes(*width) as u8,
            )?;
            write_vcpu_run_u16(control_file, abi::KVM_RUN_IO_PORT_OFFSET, port.number())?;
            write_vcpu_run_u32(control_file, abi::KVM_RUN_IO_COUNT_OFFSET, 1)?;
            write_vcpu_run_u64(
                control_file,
                abi::KVM_RUN_IO_DATA_OFFSET_OFFSET,
                abi::KVM_RUN_IO_DATA_OFFSET as u64,
            )?;
            api_control::write_mmap_area(
                mmap_area,
                abi::KVM_RUN_IO_DATA_OFFSET,
                &data.to_ne_bytes()[..access_width_bytes(*width) as usize],
            )?;
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn complete_mmio_read(
    control_file: api_control::ControlFileId,
    vcpu: &axvm::AxVCpuRef,
    pending: PendingMmioRead,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    let mut bytes = [0u8; 8];
    api_control::read_mmap_area(mmap_area, abi::KVM_RUN_MMIO_DATA_OFFSET, &mut bytes)?;
    let raw = u64::from_ne_bytes(bytes) as usize;
    let masked = raw & access_width_mask(pending.width);
    let val = if pending.signed_ext {
        sign_extend_value(masked, pending.width)
    } else {
        masked & access_width_mask(pending.reg_width)
    };
    vcpu.set_gpr(pending.reg, val);
    Ok(())
}

pub(super) fn complete_io_read(
    control_file: api_control::ControlFileId,
    vcpu: &axvm::AxVCpuRef,
    pending: PendingIoRead,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    let mut bytes = [0u8; 8];
    let len = access_width_bytes(pending.width) as usize;
    api_control::read_mmap_area(mmap_area, abi::KVM_RUN_IO_DATA_OFFSET, &mut bytes[..len])?;
    let value = u64::from_ne_bytes(bytes) as usize & access_width_mask(pending.width);
    vcpu.set_gpr(abi::X86_RAX_REG_INDEX, value);
    Ok(())
}
