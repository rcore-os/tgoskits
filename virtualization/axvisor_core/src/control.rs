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
use axvisor_api::control::{self as api_control, ControlOps};
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
/// Sets the vCPU MP state.
pub const KVM_SET_MP_STATE: u32 = iow(KVMIO, 0x99, KVM_MP_STATE_SIZE);
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

#[cfg(target_arch = "riscv64")]
const KVM_MAX_VCPUS: usize = 8;
#[cfg(not(target_arch = "riscv64"))]
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
const KVM_MP_STATE_STOPPED: u32 = 5;
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
#[cfg(target_arch = "riscv64")]
const RISCV_S_EXT_VECTOR: usize = (1usize << (usize::BITS - 1)) + 9;
const PAGE_SIZE: u64 = 4096;
const PAGE_SIZE_USIZE: usize = PAGE_SIZE as usize;

static REGISTERED: AtomicBool = AtomicBool::new(false);
static NEXT_CONTROL_FILE_ID: AtomicU64 = AtomicU64::new(1);
static CONTROL_FILES: Mutex<BTreeMap<api_control::ControlFileId, ControlFileState>> =
    Mutex::new(BTreeMap::new());
const KVM_CONTROL_OPS: ControlOps = ControlOps { open, close, ioctl };

#[derive(Clone)]
enum ControlFileState {
    System,
    Vm(VmFileState),
    Vcpu(VcpuFileState),
}

#[derive(Clone)]
struct VmFileState {
    vm: AxVMRef,
    memory_slots: BTreeMap<u32, MemorySlot>,
    ioeventfds: BTreeMap<IoEventFdKey, IoEventFd>,
    vcpu_files: BTreeMap<u32, api_control::ControlFileId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VcpuFileState {
    vm_file: api_control::ControlFileId,
    vcpu_id: u32,
    mmap_area: api_control::MmapAreaId,
    mp_state: u32,
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
    pinned_pages: api_control::PinnedUserPagesId,
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
    user_fd_ref: api_control::UserFdRefId,
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
    if REGISTERED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }

    if let Err(err) = api_control::register_endpoint(KVM_CONTROL_OPS) {
        REGISTERED.store(false, Ordering::Release);
        return Err(err);
    }

    info!("AxVisor KVM control endpoint registered: kvm");
    Ok(())
}

/// Shuts down host control endpoint state.
pub fn shutdown() -> AxResult {
    // Current host adapters publish the control endpoint for the host lifetime.
    Ok(())
}

fn open() -> AxResult<api_control::ControlFileId> {
    create_control_file(ControlFileState::System)
}

fn close(control_file: api_control::ControlFileId) -> AxResult {
    let removed = {
        let mut control_files = CONTROL_FILES.lock();
        let Some(removed) = control_files.remove(&control_file) else {
            return ax_err!(NotFound);
        };
        if let ControlFileState::Vcpu(vcpu) = &removed
            && let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&vcpu.vm_file)
        {
            vm.vcpu_files.remove(&vcpu.vcpu_id);
        }
        removed
    };

    match removed {
        ControlFileState::Vm(vm) => {
            let _ = vm.vm.shutdown();
            for ioeventfd in vm.ioeventfds.into_values() {
                let _ = api_control::release_user_fd_ref(ioeventfd.user_fd_ref);
            }
            for memory_slot in vm.memory_slots.into_values() {
                unmap_memory_slot(&vm.vm, memory_slot);
            }
            for vcpu_file in vm.vcpu_files.into_values() {
                match CONTROL_FILES.lock().remove(&vcpu_file) {
                    Some(ControlFileState::Vcpu(vcpu)) => {
                        let _ = api_control::release_mmap_area(vcpu.mmap_area);
                    }
                    Some(_) | None => {}
                }
            }
        }
        ControlFileState::Vcpu(vcpu) => {
            let _ = api_control::release_mmap_area(vcpu.mmap_area);
        }
        ControlFileState::System => {}
    }
    Ok(())
}

fn create_control_file(
    control_file_state: ControlFileState,
) -> AxResult<api_control::ControlFileId> {
    let control_file = next_control_file_id()?;
    CONTROL_FILES
        .lock()
        .insert(control_file, control_file_state);
    Ok(control_file)
}

fn next_control_file_id() -> AxResult<api_control::ControlFileId> {
    let control_file = NEXT_CONTROL_FILE_ID.fetch_add(1, Ordering::Relaxed);
    if control_file == 0 {
        return ax_err!(OutOfRange);
    }
    Ok(control_file)
}

fn control_file_state(control_file: api_control::ControlFileId) -> AxResult<ControlFileState> {
    match CONTROL_FILES.lock().get(&control_file).cloned() {
        Some(control_file_state) => Ok(control_file_state),
        None => ax_err!(NotFound),
    }
}

fn ioctl(control_file: api_control::ControlFileId, cmd: u32, arg: usize) -> AxResult<isize> {
    match control_file_state(control_file)? {
        ControlFileState::System => system_ioctl(cmd, arg),
        ControlFileState::Vm(_) => vm_ioctl(control_file, cmd, arg),
        ControlFileState::Vcpu(_) => vcpu_ioctl(control_file, cmd, arg),
    }
}

fn system_ioctl(cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_GET_API_VERSION => Ok(KVM_API_VERSION as isize),
        KVM_CHECK_EXTENSION => Ok(check_extension(arg) as isize),
        KVM_GET_VCPU_MMAP_SIZE => Ok(KVM_VCPU_MMAP_SIZE as isize),
        KVM_CREATE_VM => {
            let vm_file = create_vm_file()?;
            match api_control::create_user_fd(vm_file, KVM_CONTROL_OPS, None) {
                Ok(fd) => Ok(fd as isize),
                Err(err) => {
                    let _ = close(vm_file);
                    Err(err)
                }
            }
        }
        _ => ax_err!(Unsupported),
    }
}

fn create_vm_file() -> AxResult<api_control::ControlFileId> {
    let control_file = next_control_file_id()?;
    let vm_id = control_file_id_to_usize(control_file)?;
    let config = AxVMConfig::new_host_controlled(vm_id, format!("kvm-vm-{vm_id}"), KVM_MAX_VCPUS);
    let vm = AxVM::new(config)?;
    vm.init()?;
    vm.set_vm_status(VMStatus::Loaded);

    CONTROL_FILES.lock().insert(
        control_file,
        ControlFileState::Vm(VmFileState {
            vm,
            memory_slots: BTreeMap::new(),
            ioeventfds: BTreeMap::new(),
            vcpu_files: BTreeMap::new(),
        }),
    );
    Ok(control_file)
}

fn control_file_id_to_usize(control_file: api_control::ControlFileId) -> AxResult<usize> {
    let value = control_file as usize;
    if value as api_control::ControlFileId != control_file {
        return ax_err!(OutOfRange);
    }
    Ok(value)
}

fn vm_ioctl(control_file: api_control::ControlFileId, cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_CREATE_VCPU => create_vcpu_file(control_file, arg),
        KVM_SET_USER_MEMORY_REGION => {
            let region = read_userspace_memory_region(arg)?;
            set_user_memory_region(control_file, region)?;
            Ok(0)
        }
        KVM_IOEVENTFD => {
            let ioeventfd = read_ioeventfd(arg)?;
            update_ioeventfd(control_file, ioeventfd)?;
            Ok(0)
        }
        _ => Err(AxError::Unsupported),
    }
}

fn vcpu_ioctl(control_file: api_control::ControlFileId, cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_RUN => run_vcpu_file(control_file),
        KVM_GET_REGS => get_kvm_regs(control_file, arg),
        KVM_SET_REGS => set_kvm_regs(control_file, arg),
        KVM_GET_SREGS => get_kvm_sregs(control_file, arg),
        KVM_SET_SREGS => set_kvm_sregs(control_file, arg),
        KVM_GET_ONE_REG => get_one_reg(control_file, arg),
        KVM_SET_ONE_REG => set_one_reg(control_file, arg),
        KVM_GET_REG_LIST => get_reg_list(control_file, arg),
        KVM_INTERRUPT => kvm_interrupt(control_file, arg),
        KVM_GET_MP_STATE => get_mp_state(control_file, arg),
        KVM_SET_MP_STATE => set_mp_state(control_file, arg),
        _ => Err(AxError::Unsupported),
    }
}

fn get_mp_state(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
        return ax_err!(NotFound);
    };
    write_u32_user(arg, vcpu.mp_state)?;
    Ok(0)
}

fn set_mp_state(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let mp_state = read_u32_user(arg)?;
    if mp_state != KVM_MP_STATE_RUNNABLE && mp_state != KVM_MP_STATE_STOPPED {
        return ax_err!(Unsupported);
    }

    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vcpu.mp_state = mp_state;
    Ok(0)
}

#[cfg(target_arch = "riscv64")]
fn set_vcpu_file_mp_state_by_id(
    control_file: api_control::ControlFileId,
    vcpu_id: usize,
    mp_state: u32,
) -> AxResult {
    let vm_file = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        vcpu.vm_file
    };

    let mut control_files = CONTROL_FILES.lock();
    let target_file = {
        let Some(ControlFileState::Vm(vm)) = control_files.get(&vm_file) else {
            return ax_err!(NotFound);
        };
        vm.vcpu_files
            .get(&(vcpu_id as u32))
            .copied()
            .ok_or(AxError::InvalidInput)?
    };
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&target_file) else {
        return ax_err!(NotFound);
    };
    vcpu.mp_state = mp_state;
    Ok(())
}

fn get_vcpu(control_file: api_control::ControlFileId) -> AxResult<axvm::AxVCpuRef> {
    let (vm, vcpu_id) = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        let Some(ControlFileState::Vm(vm)) = control_files.get(&vcpu.vm_file) else {
            return ax_err!(NotFound);
        };
        (vm.vm.clone(), vcpu.vcpu_id as usize)
    };

    vm.vcpu(vcpu_id).ok_or(AxError::InvalidInput)
}

fn run_vcpu_file(control_file: api_control::ControlFileId) -> AxResult<isize> {
    let (vm, vcpu_id, vcpu, mp_state, pending_mmio_read) = {
        let mut control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        let Some(ControlFileState::Vm(vm)) = control_files.get(&vcpu.vm_file) else {
            return ax_err!(NotFound);
        };
        let vm = vm.vm.clone();
        let vcpu_id = vcpu.vcpu_id as usize;
        let mp_state = vcpu.mp_state;
        let pending_mmio_read = vcpu.pending_mmio_read;
        let Some(ControlFileState::Vcpu(vcpu_file_state)) = control_files.get_mut(&control_file)
        else {
            return ax_err!(NotFound);
        };
        vcpu_file_state.pending_mmio_read = None;
        let Some(vcpu) = vm.vcpu(vcpu_id) else {
            return ax_err!(NotFound);
        };
        (vm, vcpu_id, vcpu, mp_state, pending_mmio_read)
    };

    if mp_state == KVM_MP_STATE_STOPPED {
        write_vcpu_run_u32(control_file, KVM_RUN_EXIT_REASON_OFFSET, KVM_EXIT_INTR)?;
        axvisor_api::task::yield_now();
        return Ok(0);
    }

    if !vm.running() {
        vm.boot()?;
    }

    if let Some(pending) = pending_mmio_read {
        complete_mmio_read(control_file, &vcpu, pending)?;
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
                if signal_matching_ioeventfd(
                    control_file,
                    addr.as_usize() as u64,
                    width,
                    data,
                )? =>
            {
                internal_exits = 0;
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
                internal_exits += 1;
                if internal_exits >= KVM_RUN_MAX_INTERNAL_EXITS {
                    break KVM_EXIT_INTR;
                }
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
                internal_exits += 1;
                if internal_exits >= KVM_RUN_MAX_INTERNAL_EXITS {
                    break KVM_EXIT_INTR;
                }
            }
            exit_reason => {
                prepare_userspace_exit(control_file, &exit_reason)?;
                let kvm_reason = kvm_exit_reason(&exit_reason);
                break kvm_reason;
            }
        }
    };
    write_vcpu_run_u32(control_file, KVM_RUN_EXIT_REASON_OFFSET, exit_reason)?;
    Ok(0)
}

#[cfg(target_arch = "riscv64")]
fn handle_cpu_up(
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

#[cfg(target_arch = "riscv64")]
fn handle_send_ipi(
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

#[cfg(target_arch = "riscv64")]
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

fn get_one_reg(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let one_reg = read_one_reg(arg)?;
    let value = get_vcpu(control_file)?.get_arch_reg(one_reg.id)?;
    write_u64_user(one_reg.addr as usize, value)?;
    Ok(0)
}

fn get_kvm_regs(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_REGS_SIZE as usize];
    get_vcpu(control_file)?.get_kvm_regs(&mut bytes)?;
    api_control::copy_to_user(arg, &bytes)?;
    Ok(0)
}

fn set_kvm_regs(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_REGS_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;
    get_vcpu(control_file)?.set_kvm_regs(&bytes)?;
    Ok(0)
}

fn get_kvm_sregs(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_SREGS_SIZE as usize];
    get_vcpu(control_file)?.get_kvm_sregs(&mut bytes)?;
    api_control::copy_to_user(arg, &bytes)?;
    Ok(0)
}

fn set_kvm_sregs(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let mut bytes = [0u8; KVM_X86_SREGS_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;
    get_vcpu(control_file)?.set_kvm_sregs(&bytes)?;
    Ok(0)
}

fn set_one_reg(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let one_reg = read_one_reg(arg)?;
    let value = read_u64_user(one_reg.addr as usize)?;
    get_vcpu(control_file)?.set_arch_reg(one_reg.id, value)?;
    Ok(0)
}

fn kvm_interrupt(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let irq = read_u32_user(arg)?;
    let vector = match irq {
        #[cfg(target_arch = "riscv64")]
        KVM_INTERRUPT_SET => RISCV_S_EXT_VECTOR,
        #[cfg(not(target_arch = "riscv64"))]
        KVM_INTERRUPT_SET => 1,
        KVM_INTERRUPT_UNSET => 0,
        _ => return ax_err!(Unsupported),
    };
    get_vcpu(control_file)?.inject_interrupt(vector)?;
    Ok(0)
}

fn get_reg_list(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let vcpu = get_vcpu(control_file)?;
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
                KVM_RUN_MMIO_PHYS_ADDR_OFFSET,
                addr.as_usize() as u64,
            )?;
            write_vcpu_run_u32(
                control_file,
                KVM_RUN_MMIO_LEN_OFFSET,
                access_width_bytes(*width),
            )?;
            write_vcpu_run_u8(control_file, KVM_RUN_MMIO_IS_WRITE_OFFSET, 0)?;

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
                KVM_RUN_MMIO_PHYS_ADDR_OFFSET,
                addr.as_usize() as u64,
            )?;
            api_control::write_mmap_area(mmap_area, KVM_RUN_MMIO_DATA_OFFSET, &data.to_ne_bytes())?;
            write_vcpu_run_u32(
                control_file,
                KVM_RUN_MMIO_LEN_OFFSET,
                access_width_bytes(*width),
            )?;
            write_vcpu_run_u8(control_file, KVM_RUN_MMIO_IS_WRITE_OFFSET, 1)?;
        }
        _ => {}
    }
    Ok(())
}

fn complete_mmio_read(
    control_file: api_control::ControlFileId,
    vcpu: &axvm::AxVCpuRef,
    pending: PendingMmioRead,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    let mut bytes = [0u8; 8];
    api_control::read_mmap_area(mmap_area, KVM_RUN_MMIO_DATA_OFFSET, &mut bytes)?;
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

fn write_vcpu_run_u32(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u32,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &value.to_ne_bytes())
}

fn write_vcpu_run_u64(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u64,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &value.to_ne_bytes())
}

fn write_vcpu_run_u8(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u8,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &[value])
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

fn create_vcpu_file(control_file: api_control::ControlFileId, vcpu_id: usize) -> AxResult<isize> {
    let vcpu_id = vcpu_id as u32;
    let mmap_area = api_control::create_mmap_area(KVM_VCPU_MMAP_SIZE)?;

    let vcpu_file = {
        let mut control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
            return ax_err!(NotFound);
        };
        if vcpu_id as usize >= vm.vm.vcpu_num() {
            return ax_err!(InvalidInput);
        }
        if vm.vcpu_files.contains_key(&vcpu_id) {
            return ax_err!(AlreadyExists);
        }

        let vcpu_file = next_control_file_id()?;
        vm.vcpu_files.insert(vcpu_id, vcpu_file);
        control_files.insert(
            vcpu_file,
            ControlFileState::Vcpu(VcpuFileState {
                vm_file: control_file,
                vcpu_id,
                mmap_area,
                mp_state: if vcpu_id == 0 {
                    KVM_MP_STATE_RUNNABLE
                } else {
                    KVM_MP_STATE_STOPPED
                },
                pending_mmio_read: None,
            }),
        );
        vcpu_file
    };

    match api_control::create_user_fd(vcpu_file, KVM_CONTROL_OPS, Some(mmap_area)) {
        Ok(fd) => Ok(fd as isize),
        Err(err) => {
            let _ = remove_vcpu_file(vcpu_file);
            Err(err)
        }
    }
}

fn remove_vcpu_file(vcpu_file: api_control::ControlFileId) -> AxResult {
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.remove(&vcpu_file) else {
        return ax_err!(NotFound);
    };
    if let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&vcpu.vm_file) {
        vm.vcpu_files.remove(&vcpu.vcpu_id);
    }
    let _ = api_control::release_mmap_area(vcpu.mmap_area);
    Ok(())
}

fn control_file_mmap_area(
    control_file: api_control::ControlFileId,
) -> AxResult<api_control::MmapAreaId> {
    let control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
        return ax_err!(NotFound);
    };
    Ok(vcpu.mmap_area)
}

fn read_userspace_memory_region(arg: usize) -> AxResult<UserspaceMemoryRegion> {
    let mut bytes = [0u8; KVM_USERSPACE_MEMORY_REGION_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;

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
    api_control::copy_from_user(arg, &mut bytes)?;

    Ok(KvmIoEventFd {
        datamatch: u64::from_ne_bytes(bytes[0..8].try_into().unwrap()),
        addr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
        len: u32::from_ne_bytes(bytes[16..20].try_into().unwrap()),
        fd: i32::from_ne_bytes(bytes[20..24].try_into().unwrap()),
        flags: u32::from_ne_bytes(bytes[24..28].try_into().unwrap()),
    })
}

fn update_ioeventfd(control_file: api_control::ControlFileId, ioeventfd: KvmIoEventFd) -> AxResult {
    validate_ioeventfd(ioeventfd)?;

    let key = IoEventFdKey {
        addr: ioeventfd.addr,
        datamatch: ioeventfd.datamatch,
    };
    let user_fd_ref = if ioeventfd.flags & KVM_IOEVENTFD_FLAG_DEASSIGN == 0 {
        Some(api_control::get_user_fd_ref(ioeventfd.fd)?)
    } else {
        None
    };
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
        if let Some(user_fd_ref) = user_fd_ref {
            let _ = api_control::release_user_fd_ref(user_fd_ref);
        }
        return ax_err!(NotFound);
    };

    if ioeventfd.flags & KVM_IOEVENTFD_FLAG_DEASSIGN != 0 {
        let existing = vm.ioeventfds.remove(&key).ok_or(AxError::NotFound)?;
        let _ = api_control::release_user_fd_ref(existing.user_fd_ref);
    } else {
        if let Some(existing) = vm.ioeventfds.remove(&key) {
            let _ = api_control::release_user_fd_ref(existing.user_fd_ref);
        }
        vm.ioeventfds.insert(
            key,
            IoEventFd {
                addr: ioeventfd.addr,
                len: ioeventfd.len,
                datamatch: ioeventfd.datamatch,
                user_fd_ref: user_fd_ref.unwrap(),
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
    control_file: api_control::ControlFileId,
    addr: u64,
    width: AccessWidth,
    data: u64,
) -> AxResult<bool> {
    let ioeventfd = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        let Some(ControlFileState::Vm(vm)) = control_files.get(&vcpu.vm_file) else {
            return ax_err!(NotFound);
        };
        vm.ioeventfds
            .values()
            .find(|ioeventfd| ioeventfd_matches(ioeventfd, addr, width, data))
            .copied()
    };

    if let Some(ioeventfd) = ioeventfd {
        let written = api_control::write_user_fd_ref(ioeventfd.user_fd_ref, &1u64.to_ne_bytes())?;
        if written != core::mem::size_of::<u64>() {
            return Err(AxError::Io);
        }
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
    api_control::copy_to_user(arg, &value.to_ne_bytes())
}

fn read_u32_user(arg: usize) -> AxResult<u32> {
    let mut bytes = [0u8; 4];
    api_control::copy_from_user(arg, &mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

fn read_u64_user(arg: usize) -> AxResult<u64> {
    let mut bytes = [0u8; 8];
    api_control::copy_from_user(arg, &mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}

fn write_u64_user(arg: usize, value: u64) -> AxResult {
    api_control::copy_to_user(arg, &value.to_ne_bytes())
}

fn read_one_reg(arg: usize) -> AxResult<OneReg> {
    let mut bytes = [0u8; KVM_ONE_REG_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;

    Ok(OneReg {
        id: u64::from_ne_bytes(bytes[0..8].try_into().unwrap()),
        addr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
    })
}

fn set_user_memory_region(
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
            pinned_pages: 0,
        }
    }
}

fn map_pinned_user_memory(
    vm: &AxVMRef,
    region: UserspaceMemoryRegion,
    pinned: &api_control::PinnedUserPages,
) -> AxResult {
    let page_count = region.memory_size as usize / PAGE_SIZE_USIZE;
    if pinned.pages.len() != page_count {
        return ax_err!(InvalidInput);
    }

    let flags =
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::EXECUTE | MappingFlags::USER;
    for (mapped_pages, (page_index, page_hpa)) in pinned.pages.iter().enumerate().enumerate() {
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
    }

    Ok(())
}

fn unmap_memory_slot(vm: &AxVMRef, slot: MemorySlot) {
    let page_count = slot.memory_size as usize / PAGE_SIZE_USIZE;
    for page_index in 0..page_count {
        let page_gpa = slot.guest_phys_addr as usize + page_index * PAGE_SIZE_USIZE;
        let _ = vm.unmap_region(GuestPhysAddr::from(page_gpa), PAGE_SIZE_USIZE);
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
