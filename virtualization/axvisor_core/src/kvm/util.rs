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

use ax_errno::{AxError, AxResult, ax_err};
use axaddrspace::device::AccessWidth;
use axvisor_api::control as api_control;

use super::{CONTROL_FILES, ControlFileState};

pub(in crate::kvm) fn control_file_mmap_area(
    control_file: api_control::ControlFileId,
) -> AxResult<api_control::MmapAreaId> {
    let control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
        return ax_err!(NotFound);
    };
    Ok(vcpu.mmap_area)
}

pub(in crate::kvm) fn write_u32_user(arg: usize, value: u32) -> AxResult {
    api_control::copy_to_user(arg, &value.to_ne_bytes())
}

pub(in crate::kvm) fn read_u32_user(arg: usize) -> AxResult<u32> {
    let mut bytes = [0u8; 4];
    api_control::copy_from_user(arg, &mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

pub(in crate::kvm) fn read_u64_user(arg: usize) -> AxResult<u64> {
    let mut bytes = [0u8; 8];
    api_control::copy_from_user(arg, &mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}

pub(in crate::kvm) fn write_u64_user(arg: usize, value: u64) -> AxResult {
    api_control::copy_to_user(arg, &value.to_ne_bytes())
}

pub(in crate::kvm) fn checked_add(base: usize, offset: usize) -> AxResult<usize> {
    base.checked_add(offset).ok_or(AxError::InvalidInput)
}

pub(in crate::kvm) fn read_vcpu_run_u8(
    control_file: api_control::ControlFileId,
    offset: usize,
) -> AxResult<u8> {
    let mmap_area = control_file_mmap_area(control_file)?;
    let mut value = [0u8; 1];
    api_control::read_mmap_area(mmap_area, offset, &mut value)?;
    Ok(value[0])
}

pub(in crate::kvm) fn write_vcpu_run_u16(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u16,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &value.to_ne_bytes())
}

pub(in crate::kvm) fn write_vcpu_run_u32(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u32,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &value.to_ne_bytes())
}

pub(in crate::kvm) fn write_vcpu_run_u64(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u64,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &value.to_ne_bytes())
}

pub(in crate::kvm) fn write_vcpu_run_u8(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u8,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &[value])
}

pub(in crate::kvm) fn access_width_bytes(width: AccessWidth) -> u32 {
    match width {
        AccessWidth::Byte => 1,
        AccessWidth::Word => 2,
        AccessWidth::Dword => 4,
        AccessWidth::Qword => 8,
    }
}

pub(in crate::kvm) fn access_width_mask(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 0xff,
        AccessWidth::Word => 0xffff,
        AccessWidth::Dword => 0xffff_ffff,
        AccessWidth::Qword => usize::MAX,
    }
}

pub(in crate::kvm) fn sign_extend_value(value: usize, width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => (value as i8) as isize as usize,
        AccessWidth::Word => (value as i16) as isize as usize,
        AccessWidth::Dword => (value as i32) as isize as usize,
        AccessWidth::Qword => value,
    }
}
