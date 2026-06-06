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

//! AxVisor KVM-compatible host control endpoint callbacks.

use alloc::{collections::BTreeMap, format};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_errno::{AxError, AxResult, ax_err};
use ax_kspin::SpinNoIrq as Mutex;
use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags};
use axvisor_api::control::{self as api_control, ControlOps, EndpointSpec};
use axvm::{AxVM, AxVMRef, VMStatus, config::AxVMConfig};

const KVMIO: u32 = 0xae;

/// Current Linux KVM userspace API version.
pub const KVM_API_VERSION: usize = 12;

/// Returns [`KVM_API_VERSION`].
pub const KVM_GET_API_VERSION: u32 = ioc(KVMIO, 0x00);
/// Creates a VM fd.
pub const KVM_CREATE_VM: u32 = ioc(KVMIO, 0x01);
/// Checks whether a KVM capability is supported.
pub const KVM_CHECK_EXTENSION: u32 = ioc(KVMIO, 0x03);
/// Returns the size of the vCPU mmap area.
pub const KVM_GET_VCPU_MMAP_SIZE: u32 = ioc(KVMIO, 0x04);
/// Creates a vCPU fd on a VM fd.
pub const KVM_CREATE_VCPU: u32 = ioc(KVMIO, 0x41);
/// Configures one userspace-backed guest memory slot on a VM fd.
pub const KVM_SET_USER_MEMORY_REGION: u32 = iow(KVMIO, 0x46, KVM_USERSPACE_MEMORY_REGION_SIZE);
/// Returns the vCPU MP state.
pub const KVM_GET_MP_STATE: u32 = ior(KVMIO, 0x98, KVM_MP_STATE_SIZE);

pub const KVM_CAP_USER_MEMORY: usize = 3;
pub const KVM_CAP_NR_VCPUS: usize = 9;
pub const KVM_CAP_NR_MEMSLOTS: usize = 10;
pub const KVM_CAP_MAX_VCPUS: usize = 66;
pub const KVM_CAP_IMMEDIATE_EXIT: usize = 136;

const KVM_MAX_VCPUS: usize = 1;
const KVM_MAX_MEMORY_SLOTS: usize = 32;
const KVM_VCPU_MMAP_SIZE: usize = 0x1000;
const KVM_USERSPACE_MEMORY_REGION_SIZE: u32 = 32;
const KVM_MP_STATE_SIZE: u32 = 4;
const KVM_MP_STATE_RUNNABLE: u32 = 0;
const KVM_MEM_ALLOWED_FLAGS: u32 = 0;
const PAGE_SIZE: u64 = 4096;
const PAGE_SIZE_USIZE: usize = PAGE_SIZE as usize;

static REGISTERED: AtomicBool = AtomicBool::new(false);
static ENDPOINT_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
static SESSIONS: Mutex<BTreeMap<api_control::SessionId, Session>> = Mutex::new(BTreeMap::new());

#[derive(Clone)]
enum Session {
    System,
    Vm(VmSession),
    Vcpu(VcpuSession),
}

#[derive(Clone)]
struct VmSession {
    vm: AxVMRef,
    memory_slots: BTreeMap<u32, MemorySlot>,
    vcpu_ids: BTreeMap<u32, api_control::SessionId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VcpuSession {
    vm_session: api_control::SessionId,
    vcpu_id: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MemorySlot {
    flags: u32,
    guest_phys_addr: u64,
    memory_size: u64,
    userspace_addr: u64,
    acquired_memory: api_control::UserMemoryHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UserspaceMemoryRegion {
    slot: u32,
    flags: u32,
    guest_phys_addr: u64,
    memory_size: u64,
    userspace_addr: u64,
}

/// Registers the host-visible KVM-compatible control endpoint.
pub fn init() -> AxResult {
    if REGISTERED.swap(true, Ordering::AcqRel) {
        return Ok(());
    }

    let endpoint = api_control::register_endpoint(EndpointSpec {
        name: "kvm",
        ops: ControlOps {
            open,
            release,
            ioctl,
            read: None,
            write: None,
            poll: None,
            mmap: None,
        },
    })?;

    ENDPOINT_ID.store(endpoint, Ordering::Release);
    info!("AxVisor KVM control endpoint registered: {}", endpoint);
    Ok(())
}

/// Unregisters the host-visible KVM-compatible control endpoint.
pub fn shutdown() -> AxResult {
    if !REGISTERED.swap(false, Ordering::AcqRel) {
        return Ok(());
    }

    let endpoint = ENDPOINT_ID.swap(0, Ordering::AcqRel);
    api_control::unregister_endpoint(endpoint)
}

fn open() -> AxResult<api_control::SessionId> {
    create_session(Session::System)
}

fn release(session: api_control::SessionId) -> AxResult {
    let removed = {
        let mut sessions = SESSIONS.lock();
        let Some(removed) = sessions.remove(&session) else {
            return ax_err!(NotFound);
        };
        if let Session::Vcpu(vcpu) = &removed {
            if let Some(Session::Vm(vm)) = sessions.get_mut(&vcpu.vm_session) {
                vm.vcpu_ids.remove(&vcpu.vcpu_id);
            }
        }
        removed
    };

    if let Session::Vm(vm) = removed {
        let _ = vm.vm.shutdown();
        for memory_slot in vm.memory_slots.into_values() {
            unmap_memory_slot(&vm.vm, memory_slot);
        }
        for vcpu_session in vm.vcpu_ids.into_values() {
            let _ = SESSIONS.lock().remove(&vcpu_session);
        }
    }
    Ok(())
}

fn create_session(session_data: Session) -> AxResult<api_control::SessionId> {
    let session = next_session_id()?;
    SESSIONS.lock().insert(session, session_data);
    Ok(session)
}

fn next_session_id() -> AxResult<api_control::SessionId> {
    let session = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    if session == 0 {
        return ax_err!(OutOfRange);
    }
    Ok(session)
}

fn session_data(session: api_control::SessionId) -> AxResult<Session> {
    match SESSIONS.lock().get(&session).cloned() {
        Some(session_data) => Ok(session_data),
        None => ax_err!(NotFound),
    }
}

fn ioctl(session: api_control::SessionId, cmd: u32, arg: usize) -> AxResult<isize> {
    match session_data(session)? {
        Session::System => system_ioctl(cmd, arg),
        Session::Vm(_) => vm_ioctl(session, cmd, arg),
        Session::Vcpu(_) => vcpu_ioctl(cmd, arg),
    }
}

fn system_ioctl(cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_GET_API_VERSION => Ok(KVM_API_VERSION as isize),
        KVM_CHECK_EXTENSION => Ok(check_extension(arg) as isize),
        KVM_GET_VCPU_MMAP_SIZE => Ok(KVM_VCPU_MMAP_SIZE as isize),
        KVM_CREATE_VM => {
            let endpoint = ENDPOINT_ID.load(Ordering::Acquire);
            if endpoint == 0 {
                return ax_err!(NotFound);
            }
            let vm_session = create_vm_session()?;
            match api_control::create_vm_fd(endpoint, vm_session) {
                Ok(fd) => Ok(fd as isize),
                Err(err) => {
                    let _ = release(vm_session);
                    Err(err)
                }
            }
        }
        _ => ax_err!(Unsupported),
    }
}

fn create_vm_session() -> AxResult<api_control::SessionId> {
    let session = next_session_id()?;
    let vm_id = session_id_to_usize(session)?;
    let config = AxVMConfig::new_host_controlled(vm_id, format!("kvm-vm-{vm_id}"), KVM_MAX_VCPUS);
    let vm = AxVM::new(config)?;
    vm.init()?;
    vm.set_vm_status(VMStatus::Loaded);

    SESSIONS.lock().insert(
        session,
        Session::Vm(VmSession {
            vm,
            memory_slots: BTreeMap::new(),
            vcpu_ids: BTreeMap::new(),
        }),
    );
    Ok(session)
}

fn session_id_to_usize(session: api_control::SessionId) -> AxResult<usize> {
    let value = session as usize;
    if value as api_control::SessionId != session {
        return ax_err!(OutOfRange);
    }
    Ok(value)
}

fn vm_ioctl(session: api_control::SessionId, cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_CREATE_VCPU => create_vcpu(session, arg),
        KVM_SET_USER_MEMORY_REGION => {
            let region = read_userspace_memory_region(arg)?;
            set_user_memory_region(session, region)?;
            Ok(0)
        }
        _ => Err(AxError::Unsupported),
    }
}

fn vcpu_ioctl(cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_GET_MP_STATE => {
            write_u32_user(arg, KVM_MP_STATE_RUNNABLE)?;
            Ok(0)
        }
        _ => Err(AxError::Unsupported),
    }
}

fn check_extension(capability: usize) -> usize {
    match capability {
        KVM_CAP_USER_MEMORY => 1,
        KVM_CAP_NR_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_MAX_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_NR_MEMSLOTS => KVM_MAX_MEMORY_SLOTS,
        KVM_CAP_IMMEDIATE_EXIT => 1,
        _ => 0,
    }
}

fn create_vcpu(session: api_control::SessionId, vcpu_id: usize) -> AxResult<isize> {
    let vcpu_id = vcpu_id as u32;

    let endpoint = ENDPOINT_ID.load(Ordering::Acquire);
    if endpoint == 0 {
        return ax_err!(NotFound);
    }

    let vcpu_session = {
        let mut sessions = SESSIONS.lock();
        let Some(Session::Vm(vm)) = sessions.get_mut(&session) else {
            return ax_err!(NotFound);
        };
        if vcpu_id as usize >= vm.vm.vcpu_num() {
            return ax_err!(InvalidInput);
        }
        if vm.vcpu_ids.contains_key(&vcpu_id) {
            return ax_err!(AlreadyExists);
        }

        let vcpu_session = next_session_id()?;
        vm.vcpu_ids.insert(vcpu_id, vcpu_session);
        sessions.insert(
            vcpu_session,
            Session::Vcpu(VcpuSession {
                vm_session: session,
                vcpu_id,
            }),
        );
        vcpu_session
    };

    match api_control::create_vcpu_fd(endpoint, vcpu_session, KVM_VCPU_MMAP_SIZE) {
        Ok(fd) => Ok(fd as isize),
        Err(err) => {
            let _ = remove_vcpu_session(vcpu_session);
            Err(err)
        }
    }
}

fn remove_vcpu_session(vcpu_session: api_control::SessionId) -> AxResult {
    let mut sessions = SESSIONS.lock();
    let Some(Session::Vcpu(vcpu)) = sessions.remove(&vcpu_session) else {
        return ax_err!(NotFound);
    };
    if let Some(Session::Vm(vm)) = sessions.get_mut(&vcpu.vm_session) {
        vm.vcpu_ids.remove(&vcpu.vcpu_id);
    }
    Ok(())
}

fn read_userspace_memory_region(arg: usize) -> AxResult<UserspaceMemoryRegion> {
    let mut bytes = [0u8; KVM_USERSPACE_MEMORY_REGION_SIZE as usize];
    api_control::read_user(arg, &mut bytes)?;

    Ok(UserspaceMemoryRegion {
        slot: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
        flags: u32::from_ne_bytes(bytes[4..8].try_into().unwrap()),
        guest_phys_addr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
        memory_size: u64::from_ne_bytes(bytes[16..24].try_into().unwrap()),
        userspace_addr: u64::from_ne_bytes(bytes[24..32].try_into().unwrap()),
    })
}

fn write_u32_user(arg: usize, value: u32) -> AxResult {
    api_control::write_user(arg, &value.to_ne_bytes())
}

fn set_user_memory_region(
    session: api_control::SessionId,
    region: UserspaceMemoryRegion,
) -> AxResult {
    validate_memory_region(region)?;

    let mut sessions = SESSIONS.lock();
    let Some(Session::Vm(vm)) = sessions.get_mut(&session) else {
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
    drop(sessions);

    let acquired = api_control::acquire_user_memory(
        region.userspace_addr as usize,
        region.memory_size as usize,
        true,
    )?;
    let acquired_handle = acquired.handle;

    if let Err(err) = map_acquired_user_memory(&vm_ref, region, &acquired) {
        let _ = api_control::release_user_memory(acquired_handle);
        return Err(err);
    }

    let new_slot = MemorySlot {
        acquired_memory: acquired_handle,
        ..region.into()
    };

    let mut sessions = SESSIONS.lock();
    let Some(Session::Vm(vm)) = sessions.get_mut(&session) else {
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
    if region.slot as usize >= KVM_MAX_MEMORY_SLOTS {
        return ax_err!(InvalidInput);
    }
    if region.flags & !KVM_MEM_ALLOWED_FLAGS != 0 {
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
            acquired_memory: 0,
        }
    }
}

fn map_acquired_user_memory(
    vm: &AxVMRef,
    region: UserspaceMemoryRegion,
    acquired: &api_control::AcquiredUserMemory,
) -> AxResult {
    let page_count = region.memory_size as usize / PAGE_SIZE_USIZE;
    if acquired.pages.len() != page_count {
        return ax_err!(InvalidInput);
    }

    let flags =
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER;
    let mut mapped_pages = 0;
    for (page_index, page_hpa) in acquired.pages.iter().enumerate() {
        let page_gpa = region.guest_phys_addr as usize + page_index * PAGE_SIZE_USIZE;
        if let Err(err) = vm.map_region(
            GuestPhysAddr::from(page_gpa),
            HostPhysAddr::from(page_hpa.as_usize()),
            PAGE_SIZE_USIZE,
            flags,
        ) {
            for rollback_index in 0..mapped_pages {
                let rollback_gpa =
                    region.guest_phys_addr as usize + rollback_index * PAGE_SIZE_USIZE;
                let _ = vm.unmap_region(GuestPhysAddr::from(rollback_gpa), PAGE_SIZE_USIZE);
            }
            return Err(err);
        }
        mapped_pages += 1;
    }

    Ok(())
}

fn unmap_memory_slot(vm: &AxVMRef, slot: MemorySlot) {
    let page_count = slot.memory_size as usize / PAGE_SIZE_USIZE;
    for page_index in 0..page_count {
        let page_gpa = slot.guest_phys_addr as usize + page_index * PAGE_SIZE_USIZE;
        let _ = vm.unmap_region(GuestPhysAddr::from(page_gpa), PAGE_SIZE_USIZE);
    }
    if slot.acquired_memory != 0 {
        let _ = api_control::release_user_memory(slot.acquired_memory);
    }
}

fn ensure_no_memory_overlap(vm: &VmSession, slot_id: u32, new_slot: MemorySlot) -> AxResult {
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
    addr & (PAGE_SIZE - 1) == 0
}

const fn ioc(type_: u32, nr: u32) -> u32 {
    (type_ << 8) | nr
}

const fn iow(type_: u32, nr: u32, size: u32) -> u32 {
    const IOC_WRITE: u32 = 1;
    const IOC_TYPESHIFT: u32 = 8;
    const IOC_SIZESHIFT: u32 = 16;
    const IOC_DIRSHIFT: u32 = 30;

    (IOC_WRITE << IOC_DIRSHIFT) | (size << IOC_SIZESHIFT) | (type_ << IOC_TYPESHIFT) | nr
}

const fn ior(type_: u32, nr: u32, size: u32) -> u32 {
    const IOC_READ: u32 = 2;
    const IOC_TYPESHIFT: u32 = 8;
    const IOC_SIZESHIFT: u32 = 16;
    const IOC_DIRSHIFT: u32 = 30;

    (IOC_READ << IOC_DIRSHIFT) | (size << IOC_SIZESHIFT) | (type_ << IOC_TYPESHIFT) | nr
}
