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
use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags};
use axvisor_api::control as api_control;
use axvm::AxVMRef;

use super::{CONTROL_FILES, ControlFileState};
use crate::kvm::{
    abi::raw as abi,
    state::{MemorySlot, UserspaceMemoryRegion, VmFileState},
};

// UserspaceMemoryRegion is a plain KVM UAPI payload. MemorySlot below adds the
// host pinning handle and therefore remains local to axvisor_core.

pub(in crate::kvm) fn read_userspace_memory_region(arg: usize) -> AxResult<UserspaceMemoryRegion> {
    let mut bytes = [0u8; abi::KVM_USERSPACE_MEMORY_REGION_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;

    Ok(UserspaceMemoryRegion {
        slot: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
        flags: u32::from_ne_bytes(bytes[4..8].try_into().unwrap()),
        guest_phys_addr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
        memory_size: u64::from_ne_bytes(bytes[16..24].try_into().unwrap()),
        userspace_addr: u64::from_ne_bytes(bytes[24..32].try_into().unwrap()),
    })
}

pub(in crate::kvm) fn set_user_memory_region(
    control_file: api_control::ControlFileId,
    region: UserspaceMemoryRegion,
) -> AxResult {
    validate_memory_region(region)?;

    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };

    if region.memory_size == 0 {
        if let Some(old_slot) = vm.memory_slots.remove(&region.slot) {
            unmap_memory_slot(&vm.vm, old_slot);
        }
        return Ok(());
    }

    let vm_ref = vm.vm.clone();
    ensure_no_memory_overlap(vm, region.slot, region.into())?;
    drop(control_files);

    let pinned = api_control::pin_user_pages(
        region.userspace_addr as usize,
        region.memory_size as usize,
        true,
    )?;
    let pinned_pages = pinned.id;

    if let Err(err) = map_pinned_user_memory(&vm_ref, region, &pinned) {
        let _ = api_control::release_pinned_user_pages(pinned_pages);
        return Err(err);
    }

    let new_slot = MemorySlot {
        pinned_pages,
        ..region.into()
    };

    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
        unmap_memory_slot(&vm_ref, new_slot);
        return ax_err!(NotFound);
    };
    ensure_no_memory_overlap(vm, region.slot, new_slot)?;
    if let Some(old_slot) = vm.memory_slots.insert(region.slot, new_slot) {
        unmap_memory_slot(&vm.vm, old_slot);
    }
    Ok(())
}

fn validate_memory_region(region: UserspaceMemoryRegion) -> AxResult {
    if region.slot as usize >= abi::KVM_MAX_MEMORY_SLOTS {
        return ax_err!(InvalidInput);
    }
    if region.flags & !abi::KVM_MEM_ALLOWED_FLAGS != 0 {
        return ax_err!(InvalidInput);
    }
    if !is_page_aligned(region.guest_phys_addr)
        || !is_page_aligned(region.memory_size)
        || (region.memory_size != 0 && !is_page_aligned(region.userspace_addr))
    {
        return ax_err!(InvalidInput);
    }
    region
        .guest_phys_addr
        .checked_add(region.memory_size)
        .ok_or(AxError::InvalidInput)?;
    region
        .userspace_addr
        .checked_add(region.memory_size)
        .ok_or(AxError::InvalidInput)?;
    Ok(())
}

impl From<UserspaceMemoryRegion> for MemorySlot {
    fn from(region: UserspaceMemoryRegion) -> Self {
        Self {
            flags: region.flags,
            guest_phys_addr: region.guest_phys_addr,
            memory_size: region.memory_size,
            userspace_addr: region.userspace_addr,
            pinned_pages: 0,
        }
    }
}

fn map_pinned_user_memory(
    vm: &AxVMRef,
    region: UserspaceMemoryRegion,
    pinned: &api_control::PinnedUserPages,
) -> AxResult {
    let page_count = region.memory_size as usize / abi::PAGE_SIZE_USIZE;
    if pinned.pages.len() != page_count {
        return ax_err!(InvalidInput);
    }

    let flags =
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER;
    for (mapped_pages, (page_index, page_hpa)) in pinned.pages.iter().enumerate().enumerate() {
        let page_gpa = region.guest_phys_addr as usize + page_index * abi::PAGE_SIZE_USIZE;
        if let Err(err) = vm.map_region(
            GuestPhysAddr::from(page_gpa),
            HostPhysAddr::from(page_hpa.as_usize()),
            abi::PAGE_SIZE_USIZE,
            flags,
        ) {
            for rollback_index in 0..mapped_pages {
                let rollback_gpa =
                    region.guest_phys_addr as usize + rollback_index * abi::PAGE_SIZE_USIZE;
                let _ = vm.unmap_region(GuestPhysAddr::from(rollback_gpa), abi::PAGE_SIZE_USIZE);
            }
            return Err(err);
        }
    }

    Ok(())
}

pub(in crate::kvm) fn unmap_memory_slot(vm: &AxVMRef, slot: MemorySlot) {
    let page_count = slot.memory_size as usize / abi::PAGE_SIZE_USIZE;
    for page_index in 0..page_count {
        let page_gpa = slot.guest_phys_addr as usize + page_index * abi::PAGE_SIZE_USIZE;
        let _ = vm.unmap_region(GuestPhysAddr::from(page_gpa), abi::PAGE_SIZE_USIZE);
    }
    if slot.pinned_pages != 0 {
        let _ = api_control::release_pinned_user_pages(slot.pinned_pages);
    }
}

fn ensure_no_memory_overlap(vm: &VmFileState, slot_id: u32, new_slot: MemorySlot) -> AxResult {
    let new_end = new_slot
        .guest_phys_addr
        .checked_add(new_slot.memory_size)
        .ok_or(AxError::InvalidInput)?;

    for (&existing_slot_id, existing_slot) in vm.memory_slots.iter() {
        if existing_slot_id == slot_id {
            continue;
        }

        let existing_end = existing_slot
            .guest_phys_addr
            .checked_add(existing_slot.memory_size)
            .ok_or(AxError::InvalidInput)?;
        if new_slot.guest_phys_addr < existing_end && existing_slot.guest_phys_addr < new_end {
            return ax_err!(InvalidInput);
        }
    }

    Ok(())
}

const fn is_page_aligned(addr: u64) -> bool {
    addr & (abi::PAGE_SIZE - 1) == 0
}
