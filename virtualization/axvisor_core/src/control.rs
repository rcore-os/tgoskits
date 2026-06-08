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
use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags, device::AccessWidth};
use axvcpu::AxVCpuExitReason;
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
/// Registers or unregisters an eventfd for guest I/O writes.
pub const KVM_IOEVENTFD: u32 = iow(KVMIO, 0x79, KVM_IOEVENTFD_SIZE);
/// Runs a vCPU until it exits to userspace.
pub const KVM_RUN: u32 = ioc(KVMIO, 0x80);
/// Gets x86 general-purpose register state.
pub const KVM_GET_REGS: u32 = ior(KVMIO, 0x81, KVM_X86_REGS_SIZE);
/// Sets x86 general-purpose register state.
pub const KVM_SET_REGS: u32 = iow(KVMIO, 0x82, KVM_X86_REGS_SIZE);
/// Gets x86 special register state.
pub const KVM_GET_SREGS: u32 = ior(KVMIO, 0x83, KVM_X86_SREGS_SIZE);
/// Sets x86 special register state.
pub const KVM_SET_SREGS: u32 = iow(KVMIO, 0x84, KVM_X86_SREGS_SIZE);
/// Injects or clears an architecture-specific vCPU interrupt.
pub const KVM_INTERRUPT: u32 = iow(KVMIO, 0x86, KVM_INTERRUPT_SIZE);
/// Returns the vCPU MP state.
pub const KVM_GET_MP_STATE: u32 = ior(KVMIO, 0x98, KVM_MP_STATE_SIZE);
/// Gets one architecture-specific vCPU register.
pub const KVM_GET_ONE_REG: u32 = iow(KVMIO, 0xab, KVM_ONE_REG_SIZE);
/// Sets one architecture-specific vCPU register.
pub const KVM_SET_ONE_REG: u32 = iow(KVMIO, 0xac, KVM_ONE_REG_SIZE);
/// Gets the architecture-specific vCPU register IDs supported by this vCPU.
pub const KVM_GET_REG_LIST: u32 = iowr(KVMIO, 0xb0, KVM_REG_LIST_SIZE);

pub const KVM_CAP_USER_MEMORY: usize = 3;
pub const KVM_CAP_IOEVENTFD: usize = 36;
pub const KVM_CAP_NR_VCPUS: usize = 9;
pub const KVM_CAP_NR_MEMSLOTS: usize = 10;
pub const KVM_CAP_MAX_VCPUS: usize = 66;
pub const KVM_CAP_ONE_REG: usize = 70;
pub const KVM_CAP_IMMEDIATE_EXIT: usize = 136;

const KVM_MAX_VCPUS: usize = 1;
const KVM_MAX_MEMORY_SLOTS: usize = 32;
const KVM_VCPU_MMAP_SIZE: usize = 0x1000;
const KVM_USERSPACE_MEMORY_REGION_SIZE: u32 = 32;
const KVM_IOEVENTFD_SIZE: u32 = 64;
const KVM_INTERRUPT_SIZE: u32 = 4;
const KVM_MP_STATE_SIZE: u32 = 4;
const KVM_ONE_REG_SIZE: u32 = 16;
const KVM_REG_LIST_SIZE: u32 = 8;
const KVM_X86_REGS_SIZE: u32 = 18 * 8;
const KVM_X86_SREGS_SIZE: u32 = 312;
const KVM_MP_STATE_RUNNABLE: u32 = 0;
const KVM_MEM_ALLOWED_FLAGS: u32 = 0;
const KVM_IOEVENTFD_FLAG_DATAMATCH: u32 = 1 << 0;
const KVM_IOEVENTFD_FLAG_PIO: u32 = 1 << 1;
const KVM_IOEVENTFD_FLAG_DEASSIGN: u32 = 1 << 2;
const KVM_IOEVENTFD_VALID_FLAGS: u32 =
    KVM_IOEVENTFD_FLAG_DATAMATCH | KVM_IOEVENTFD_FLAG_PIO | KVM_IOEVENTFD_FLAG_DEASSIGN;
const KVM_INTERRUPT_SET: u32 = u32::MAX;
const KVM_INTERRUPT_UNSET: u32 = u32::MAX - 1;
const KVM_RUN_EXIT_REASON_OFFSET: usize = 8;
const KVM_RUN_MMIO_PHYS_ADDR_OFFSET: usize = 32;
const KVM_RUN_MMIO_DATA_OFFSET: usize = 40;
const KVM_RUN_MMIO_LEN_OFFSET: usize = 48;
const KVM_RUN_MMIO_IS_WRITE_OFFSET: usize = 52;
const KVM_EXIT_UNKNOWN: u32 = 0;
const KVM_EXIT_HLT: u32 = 5;
const KVM_EXIT_MMIO: u32 = 6;
const KVM_EXIT_SHUTDOWN: u32 = 8;
const KVM_EXIT_FAIL_ENTRY: u32 = 9;
const KVM_EXIT_INTR: u32 = 10;
const KVM_EXIT_INTERNAL_ERROR: u32 = 17;
const KVM_EXIT_MEMORY_FAULT: u32 = 39;
const KVM_RUN_MAX_INTERNAL_EXITS: usize = 1024;
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
    ioeventfds: BTreeMap<IoEventFdKey, IoEventFd>,
    vcpu_ids: BTreeMap<u32, api_control::SessionId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VcpuSession {
    vm_session: api_control::SessionId,
    vcpu_id: u32,
    pending_mmio_read: Option<PendingMmioRead>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingMmioRead {
    reg: usize,
    width: axaddrspace::device::AccessWidth,
    reg_width: axaddrspace::device::AccessWidth,
    signed_ext: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MemorySlot {
    flags: u32,
    guest_phys_addr: u64,
    memory_size: u64,
    userspace_addr: u64,
    acquired_memory: api_control::UserMemoryHandle,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct IoEventFdKey {
    addr: u64,
    datamatch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IoEventFd {
    addr: u64,
    len: u32,
    datamatch: u64,
    fd: api_control::HostFd,
    flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UserspaceMemoryRegion {
    slot: u32,
    flags: u32,
    guest_phys_addr: u64,
    memory_size: u64,
    userspace_addr: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OneReg {
    id: u64,
    addr: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KvmIoEventFd {
    datamatch: u64,
    addr: u64,
    len: u32,
    fd: i32,
    flags: u32,
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
        Session::Vcpu(_) => vcpu_ioctl(session, cmd, arg),
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
            ioeventfds: BTreeMap::new(),
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
        KVM_IOEVENTFD => {
            let ioeventfd = read_ioeventfd(arg)?;
            update_ioeventfd(session, ioeventfd)?;
            Ok(0)
        }
        _ => Err(AxError::Unsupported),
    }
}

fn vcpu_ioctl(session: api_control::SessionId, cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_RUN => run_vcpu(session),
        KVM_GET_REGS => get_kvm_regs(session, arg),
        KVM_SET_REGS => set_kvm_regs(session, arg),
        KVM_GET_SREGS => get_kvm_sregs(session, arg),
        KVM_SET_SREGS => set_kvm_sregs(session, arg),
        KVM_GET_ONE_REG => get_one_reg(session, arg),
        KVM_SET_ONE_REG => set_one_reg(session, arg),
        KVM_GET_REG_LIST => get_reg_list(session, arg),
        KVM_INTERRUPT => kvm_interrupt(session, arg),
        KVM_GET_MP_STATE => {
            write_u32_user(arg, KVM_MP_STATE_RUNNABLE)?;
            Ok(0)
        }
        _ => Err(AxError::Unsupported),
    }
}

fn get_vcpu(session: api_control::SessionId) -> AxResult<axvm::AxVCpuRef> {
    let (vm, vcpu_id) = {
        let sessions = SESSIONS.lock();
        let Some(Session::Vcpu(vcpu)) = sessions.get(&session) else {
            return ax_err!(NotFound);
        };
        let Some(Session::Vm(vm)) = sessions.get(&vcpu.vm_session) else {
            return ax_err!(NotFound);
        };
        (vm.vm.clone(), vcpu.vcpu_id as usize)
    };

    vm.vcpu(vcpu_id).ok_or(AxError::InvalidInput)
}

fn run_vcpu(session: api_control::SessionId) -> AxResult<isize> {
    let (vm, vcpu_id, vcpu, pending_mmio_read) = {
        let mut sessions = SESSIONS.lock();
        let Some(Session::Vcpu(vcpu)) = sessions.get(&session) else {
            return ax_err!(NotFound);
        };
        let Some(Session::Vm(vm)) = sessions.get(&vcpu.vm_session) else {
            return ax_err!(NotFound);
        };
        let vm = vm.vm.clone();
        let vcpu_id = vcpu.vcpu_id as usize;
        let pending_mmio_read = vcpu.pending_mmio_read;
        let Some(Session::Vcpu(vcpu_session)) = sessions.get_mut(&session) else {
            return ax_err!(NotFound);
        };
        vcpu_session.pending_mmio_read = None;
        let Some(vcpu) = vm.vcpu(vcpu_id) else {
            return ax_err!(NotFound);
        };
        (vm, vcpu_id, vcpu, pending_mmio_read)
    };

    if !vm.running() {
        vm.boot()?;
    }

    if let Some(pending) = pending_mmio_read {
        complete_mmio_read(session, &vcpu, pending)?;
    }

    let mut internal_exits = 0;
    let exit_reason = loop {
        let exit_reason = match vm.run_vcpu_raw(vcpu_id) {
            Ok(exit_reason) => exit_reason,
            Err(err) => {
                warn!("KVM_RUN vCPU error: {:?}", err);
                break KVM_EXIT_INTERNAL_ERROR;
            }
        };

        match exit_reason {
            AxVCpuExitReason::Nothing
            | AxVCpuExitReason::PreemptionTimer
            | AxVCpuExitReason::ExternalInterrupt { .. }
            | AxVCpuExitReason::InterruptEnd { .. } => {
                crate::vmm::vcpus::handle_internal_exit(&vm, &vcpu, &exit_reason);
                axvisor_api::task::yield_now();
                internal_exits += 1;
                if internal_exits >= KVM_RUN_MAX_INTERNAL_EXITS {
                    break KVM_EXIT_INTR;
                }
            }
            AxVCpuExitReason::MmioWrite { addr, width, data }
                if signal_matching_ioeventfd(session, addr.as_usize() as u64, width, data)? =>
            {
                internal_exits = 0;
            }
            exit_reason => {
                prepare_userspace_exit(session, &exit_reason)?;
                let kvm_reason = kvm_exit_reason(&exit_reason);
                break kvm_reason;
            }
        }
    };
    write_vcpu_run_u32(session, KVM_RUN_EXIT_REASON_OFFSET, exit_reason)?;
    Ok(0)
}

fn get_one_reg(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let one_reg = read_one_reg(arg)?;
    let value = get_vcpu(session)?.get_arch_reg(one_reg.id)?;
    write_u64_user(one_reg.addr as usize, value)?;
    Ok(0)
}

fn get_kvm_regs(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_REGS_SIZE as usize];
    get_vcpu(session)?.get_kvm_regs(&mut bytes)?;
    api_control::write_user(arg, &bytes)?;
    Ok(0)
}

fn set_kvm_regs(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_REGS_SIZE as usize];
    api_control::read_user(arg, &mut bytes)?;
    get_vcpu(session)?.set_kvm_regs(&bytes)?;
    Ok(0)
}

fn get_kvm_sregs(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_SREGS_SIZE as usize];
    get_vcpu(session)?.get_kvm_sregs(&mut bytes)?;
    api_control::write_user(arg, &bytes)?;
    Ok(0)
}

fn set_kvm_sregs(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_SREGS_SIZE as usize];
    api_control::read_user(arg, &mut bytes)?;
    get_vcpu(session)?.set_kvm_sregs(&bytes)?;
    Ok(0)
}

fn set_one_reg(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let one_reg = read_one_reg(arg)?;
    let value = read_u64_user(one_reg.addr as usize)?;
    get_vcpu(session)?.set_arch_reg(one_reg.id, value)?;
    Ok(0)
}

fn kvm_interrupt(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let irq = read_u32_user(arg)?;
    let vector = match irq {
        KVM_INTERRUPT_SET => 1,
        KVM_INTERRUPT_UNSET => 0,
        _ => return ax_err!(Unsupported),
    };
    get_vcpu(session)?.inject_interrupt(vector)?;
    Ok(0)
}

fn get_reg_list(session: api_control::SessionId, arg: usize) -> AxResult<isize> {
    let vcpu = get_vcpu(session)?;
    let reg_ids = vcpu.arch_reg_ids();
    let requested = read_u64_user(arg)? as usize;
    write_u64_user(arg, reg_ids.len() as u64)?;
    if requested < reg_ids.len() {
        return ax_err!(ArgumentListTooLong);
    }

    let mut offset = arg.checked_add(8).ok_or(AxError::InvalidInput)?;
    for reg_id in reg_ids {
        write_u64_user(offset, *reg_id)?;
        offset = offset.checked_add(8).ok_or(AxError::InvalidInput)?;
    }
    Ok(0)
}

fn kvm_exit_reason(exit_reason: &AxVCpuExitReason) -> u32 {
    match exit_reason {
        AxVCpuExitReason::Halt => KVM_EXIT_HLT,
        AxVCpuExitReason::MmioRead { .. } | AxVCpuExitReason::MmioWrite { .. } => KVM_EXIT_MMIO,
        AxVCpuExitReason::NestedPageFault { .. } => KVM_EXIT_MEMORY_FAULT,
        AxVCpuExitReason::SystemDown => KVM_EXIT_SHUTDOWN,
        AxVCpuExitReason::FailEntry { .. } => KVM_EXIT_FAIL_ENTRY,
        AxVCpuExitReason::ExternalInterrupt { .. } | AxVCpuExitReason::PreemptionTimer => {
            KVM_EXIT_INTR
        }
        _ => KVM_EXIT_UNKNOWN,
    }
}

fn prepare_userspace_exit(
    session: api_control::SessionId,
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
                session,
                KVM_RUN_MMIO_PHYS_ADDR_OFFSET,
                addr.as_usize() as u64,
            )?;
            write_vcpu_run_u32(session, KVM_RUN_MMIO_LEN_OFFSET, access_width_bytes(*width))?;
            write_vcpu_run_u8(session, KVM_RUN_MMIO_IS_WRITE_OFFSET, 0)?;

            let mut sessions = SESSIONS.lock();
            let Some(Session::Vcpu(vcpu)) = sessions.get_mut(&session) else {
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
            write_vcpu_run_u64(
                session,
                KVM_RUN_MMIO_PHYS_ADDR_OFFSET,
                addr.as_usize() as u64,
            )?;
            api_control::write_vcpu_run_page(
                session,
                KVM_RUN_MMIO_DATA_OFFSET,
                &data.to_ne_bytes(),
            )?;
            write_vcpu_run_u32(session, KVM_RUN_MMIO_LEN_OFFSET, access_width_bytes(*width))?;
            write_vcpu_run_u8(session, KVM_RUN_MMIO_IS_WRITE_OFFSET, 1)?;
        }
        _ => {}
    }
    Ok(())
}

fn complete_mmio_read(
    session: api_control::SessionId,
    vcpu: &axvm::AxVCpuRef,
    pending: PendingMmioRead,
) -> AxResult {
    let mut bytes = [0u8; 8];
    api_control::read_vcpu_run_page(session, KVM_RUN_MMIO_DATA_OFFSET, &mut bytes)?;
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

fn access_width_bytes(width: AccessWidth) -> u32 {
    match width {
        AccessWidth::Byte => 1,
        AccessWidth::Word => 2,
        AccessWidth::Dword => 4,
        AccessWidth::Qword => 8,
    }
}

fn access_width_mask(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 0xff,
        AccessWidth::Word => 0xffff,
        AccessWidth::Dword => 0xffff_ffff,
        AccessWidth::Qword => usize::MAX,
    }
}

fn sign_extend_value(value: usize, width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => (value as i8) as isize as usize,
        AccessWidth::Word => (value as i16) as isize as usize,
        AccessWidth::Dword => (value as i32) as isize as usize,
        AccessWidth::Qword => value,
    }
}

fn write_vcpu_run_u32(session: api_control::SessionId, offset: usize, value: u32) -> AxResult {
    api_control::write_vcpu_run_page(session, offset, &value.to_ne_bytes())
}

fn write_vcpu_run_u64(session: api_control::SessionId, offset: usize, value: u64) -> AxResult {
    api_control::write_vcpu_run_page(session, offset, &value.to_ne_bytes())
}

fn write_vcpu_run_u8(session: api_control::SessionId, offset: usize, value: u8) -> AxResult {
    api_control::write_vcpu_run_page(session, offset, &[value])
}

fn check_extension(capability: usize) -> usize {
    match capability {
        KVM_CAP_USER_MEMORY => 1,
        KVM_CAP_IOEVENTFD => 1,
        KVM_CAP_NR_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_MAX_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_NR_MEMSLOTS => KVM_MAX_MEMORY_SLOTS,
        KVM_CAP_ONE_REG => usize::from(cfg!(target_arch = "riscv64")),
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
                pending_mmio_read: None,
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

fn read_ioeventfd(arg: usize) -> AxResult<KvmIoEventFd> {
    let mut bytes = [0u8; KVM_IOEVENTFD_SIZE as usize];
    api_control::read_user(arg, &mut bytes)?;

    Ok(KvmIoEventFd {
        datamatch: u64::from_ne_bytes(bytes[0..8].try_into().unwrap()),
        addr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
        len: u32::from_ne_bytes(bytes[16..20].try_into().unwrap()),
        fd: i32::from_ne_bytes(bytes[20..24].try_into().unwrap()),
        flags: u32::from_ne_bytes(bytes[24..28].try_into().unwrap()),
    })
}

fn update_ioeventfd(session: api_control::SessionId, ioeventfd: KvmIoEventFd) -> AxResult {
    validate_ioeventfd(ioeventfd)?;

    let key = IoEventFdKey {
        addr: ioeventfd.addr,
        datamatch: ioeventfd.datamatch,
    };
    let mut sessions = SESSIONS.lock();
    let Some(Session::Vm(vm)) = sessions.get_mut(&session) else {
        return ax_err!(NotFound);
    };

    if ioeventfd.flags & KVM_IOEVENTFD_FLAG_DEASSIGN != 0 {
        vm.ioeventfds.remove(&key).ok_or(AxError::NotFound)?;
    } else {
        vm.ioeventfds.insert(
            key,
            IoEventFd {
                addr: ioeventfd.addr,
                len: ioeventfd.len,
                datamatch: ioeventfd.datamatch,
                fd: ioeventfd.fd,
                flags: ioeventfd.flags,
            },
        );
    }
    Ok(())
}

fn validate_ioeventfd(ioeventfd: KvmIoEventFd) -> AxResult {
    if ioeventfd.flags & !KVM_IOEVENTFD_VALID_FLAGS != 0 {
        return ax_err!(InvalidInput);
    }
    if ioeventfd.flags & KVM_IOEVENTFD_FLAG_PIO != 0 {
        return ax_err!(Unsupported);
    }
    if !matches!(ioeventfd.len, 1 | 2 | 4 | 8) {
        return ax_err!(InvalidInput);
    }
    if ioeventfd.fd < 0 {
        return ax_err!(InvalidInput);
    }
    Ok(())
}

fn signal_matching_ioeventfd(
    session: api_control::SessionId,
    addr: u64,
    width: AccessWidth,
    data: u64,
) -> AxResult<bool> {
    let ioeventfd = {
        let sessions = SESSIONS.lock();
        let Some(Session::Vcpu(vcpu)) = sessions.get(&session) else {
            return ax_err!(NotFound);
        };
        let Some(Session::Vm(vm)) = sessions.get(&vcpu.vm_session) else {
            return ax_err!(NotFound);
        };
        vm.ioeventfds
            .values()
            .find(|ioeventfd| ioeventfd_matches(ioeventfd, addr, width, data))
            .copied()
    };

    if let Some(ioeventfd) = ioeventfd {
        api_control::signal_eventfd(ioeventfd.fd)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn ioeventfd_matches(ioeventfd: &IoEventFd, addr: u64, width: AccessWidth, data: u64) -> bool {
    if ioeventfd.addr != addr || ioeventfd.len != access_width_bytes(width) {
        return false;
    }
    if ioeventfd.flags & KVM_IOEVENTFD_FLAG_DATAMATCH == 0 {
        return true;
    }
    let mask = access_width_mask(width) as u64;
    (data & mask) == (ioeventfd.datamatch & mask)
}

fn write_u32_user(arg: usize, value: u32) -> AxResult {
    api_control::write_user(arg, &value.to_ne_bytes())
}

fn read_u32_user(arg: usize) -> AxResult<u32> {
    let mut bytes = [0u8; 4];
    api_control::read_user(arg, &mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

fn read_u64_user(arg: usize) -> AxResult<u64> {
    let mut bytes = [0u8; 8];
    api_control::read_user(arg, &mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}

fn write_u64_user(arg: usize, value: u64) -> AxResult {
    api_control::write_user(arg, &value.to_ne_bytes())
}

fn read_one_reg(arg: usize) -> AxResult<OneReg> {
    let mut bytes = [0u8; KVM_ONE_REG_SIZE as usize];
    api_control::read_user(arg, &mut bytes)?;

    Ok(OneReg {
        id: u64::from_ne_bytes(bytes[0..8].try_into().unwrap()),
        addr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
    })
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

const fn iowr(type_: u32, nr: u32, size: u32) -> u32 {
    const IOC_WRITE: u32 = 1;
    const IOC_READ: u32 = 2;
    const IOC_TYPESHIFT: u32 = 8;
    const IOC_SIZESHIFT: u32 = 16;
    const IOC_DIRSHIFT: u32 = 30;

    ((IOC_WRITE | IOC_READ) << IOC_DIRSHIFT)
        | (size << IOC_SIZESHIFT)
        | (type_ << IOC_TYPESHIFT)
        | nr
}
