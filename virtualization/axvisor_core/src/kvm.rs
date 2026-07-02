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

use alloc::{collections::BTreeMap, format, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_errno::{AxError, AxErrorKind, AxResult, ax_err};
use ax_kspin::SpinNoIrq as Mutex;
use axaddrspace::{GuestPhysAddr, HostPhysAddr, MappingFlags, device::AccessWidth};
use axvcpu::AxVCpuExitReason;
use axvisor_api::{
    control::{self as api_control, ControlOps},
    task::{self as api_task, TaskHandle, TaskOptions},
};
use axvm::{AxVM, AxVMRef, VMStatus, config::AxVMConfig};

const KVMIO: u32 = 0xae;

/// Current Linux KVM userspace API version.
pub const KVM_API_VERSION: usize = 12;

/// Returns [`KVM_API_VERSION`].
pub const KVM_GET_API_VERSION: u32 = ioc(KVMIO, 0x00);
/// Creates a VM fd.
pub const KVM_CREATE_VM: u32 = ioc(KVMIO, 0x01);
/// Returns the x86 MSR indices supported by this KVM-compatible endpoint.
pub const KVM_GET_MSR_INDEX_LIST: u32 = iowr(KVMIO, 0x02, KVM_MSR_LIST_SIZE);
/// Checks whether a KVM capability is supported.
pub const KVM_CHECK_EXTENSION: u32 = ioc(KVMIO, 0x03);
/// Returns the size of the vCPU mmap area.
pub const KVM_GET_VCPU_MMAP_SIZE: u32 = ioc(KVMIO, 0x04);
/// Returns the x86 CPUID entries supported by this KVM-compatible endpoint.
pub const KVM_GET_SUPPORTED_CPUID: u32 = iowr(KVMIO, 0x05, KVM_CPUID2_SIZE);
/// Creates a vCPU fd on a VM fd.
pub const KVM_CREATE_VCPU: u32 = ioc(KVMIO, 0x41);
/// Configures one userspace-backed guest memory slot on a VM fd.
pub const KVM_SET_USER_MEMORY_REGION: u32 = iow(KVMIO, 0x46, KVM_USERSPACE_MEMORY_REGION_SIZE);
/// Sets the x86 TSS address.
pub const KVM_SET_TSS_ADDR: u32 = ioc(KVMIO, 0x47);
/// Creates an in-kernel x86 IRQ chip.
pub const KVM_CREATE_IRQCHIP: u32 = ioc(KVMIO, 0x60);
/// Sets x86 GSI routing.
pub const KVM_SET_GSI_ROUTING: u32 = iow(KVMIO, 0x6a, KVM_IRQ_ROUTING_SIZE);
/// Registers or unregisters an eventfd as an x86 interrupt source.
pub const KVM_IRQFD: u32 = iow(KVMIO, 0x76, KVM_IRQFD_SIZE);
/// Creates an in-kernel x86 PIT.
pub const KVM_CREATE_PIT2: u32 = iow(KVMIO, 0x77, KVM_PIT_CONFIG_SIZE);
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
/// Gets x86 MSR state.
pub const KVM_GET_MSRS: u32 = iowr(KVMIO, 0x88, KVM_MSRS_SIZE);
/// Sets x86 MSR state.
pub const KVM_SET_MSRS: u32 = iow(KVMIO, 0x89, KVM_MSRS_SIZE);
/// Sets x86 FPU state.
pub const KVM_SET_FPU: u32 = iow(KVMIO, 0x8d, KVM_X86_FPU_SIZE);
/// Gets x86 LAPIC state.
pub const KVM_GET_LAPIC: u32 = ior(KVMIO, 0x8e, KVM_X86_LAPIC_STATE_SIZE);
/// Sets x86 LAPIC state.
pub const KVM_SET_LAPIC: u32 = iow(KVMIO, 0x8f, KVM_X86_LAPIC_STATE_SIZE);
/// Sets x86 CPUID entries.
pub const KVM_SET_CPUID2: u32 = iow(KVMIO, 0x90, KVM_CPUID2_SIZE);
/// Gets x86 CPUID entries configured on this vCPU.
pub const KVM_GET_CPUID2: u32 = iowr(KVMIO, 0x91, KVM_CPUID2_SIZE);
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

pub const KVM_CAP_IRQCHIP: usize = 0;
pub const KVM_CAP_USER_MEMORY: usize = 3;
pub const KVM_CAP_SET_TSS_ADDR: usize = 4;
pub const KVM_CAP_EXT_CPUID: usize = 7;
pub const KVM_CAP_NR_VCPUS: usize = 9;
pub const KVM_CAP_NR_MEMSLOTS: usize = 10;
pub const KVM_CAP_MP_STATE: usize = 14;
pub const KVM_CAP_IRQFD: usize = 32;
pub const KVM_CAP_PIT2: usize = 33;
pub const KVM_CAP_PIT_STATE2: usize = 35;
pub const KVM_CAP_IOEVENTFD: usize = 36;
pub const KVM_CAP_ADJUST_CLOCK: usize = 39;
pub const KVM_CAP_VCPU_EVENTS: usize = 41;
pub const KVM_CAP_DEBUGREGS: usize = 50;
pub const KVM_CAP_XSAVE: usize = 55;
pub const KVM_CAP_XCRS: usize = 56;
pub const KVM_CAP_MAX_VCPUS: usize = 66;
pub const KVM_CAP_ONE_REG: usize = 70;
pub const KVM_CAP_IMMEDIATE_EXIT: usize = 136;
pub const KVM_CAP_XSAVE2: usize = 208;

#[cfg(target_arch = "riscv64")]
const KVM_MAX_VCPUS: usize = 8;
#[cfg(not(target_arch = "riscv64"))]
const KVM_MAX_VCPUS: usize = 1;
const KVM_MAX_MEMORY_SLOTS: usize = 32;
const KVM_MAX_CPUID_ENTRIES: usize = 256;
const KVM_MAX_MSR_ENTRIES: usize = 256;
const KVM_VCPU_MMAP_SIZE: usize = 0x1000;
const KVM_MSR_LIST_SIZE: u32 = 4;
const KVM_CPUID2_SIZE: u32 = 8;
const KVM_CPUID_ENTRY2_SIZE: usize = 40;
const KVM_MSRS_SIZE: u32 = 8;
const KVM_MSR_ENTRY_SIZE: usize = 16;
const KVM_USERSPACE_MEMORY_REGION_SIZE: u32 = 32;
const KVM_IOEVENTFD_SIZE: u32 = 64;
const KVM_IRQ_ROUTING_SIZE: u32 = 8;
const KVM_IRQ_ROUTING_ENTRY_SIZE: usize = 48;
const KVM_MAX_IRQ_ROUTES: usize = 4096;
const KVM_IRQ_ROUTING_IRQCHIP: u32 = 1;
const KVM_IRQ_ROUTING_MSI: u32 = 2;
const KVM_IRQFD_SIZE: u32 = 32;
const KVM_IRQFD_FLAG_DEASSIGN: u32 = 1 << 0;
const KVM_IRQFD_FLAG_RESAMPLE: u32 = 1 << 1;
const KVM_IRQFD_VALID_FLAGS: u32 = KVM_IRQFD_FLAG_DEASSIGN | KVM_IRQFD_FLAG_RESAMPLE;
const KVM_PIT_CONFIG_SIZE: u32 = 64;
const KVM_PIT_VALID_FLAGS: u32 = 1;
const KVM_INTERRUPT_SIZE: u32 = 4;
const KVM_MP_STATE_SIZE: u32 = 4;
const KVM_ONE_REG_SIZE: u32 = 16;
const KVM_REG_LIST_SIZE: u32 = 8;
const KVM_X86_REGS_SIZE: u32 = 18 * 8;
const KVM_X86_SREGS_SIZE: u32 = 312;
const KVM_X86_FPU_SIZE: u32 = 416;
const KVM_X86_LAPIC_STATE_SIZE: u32 = 1024;
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
const KVM_RUN_IO_DIRECTION_OFFSET: usize = 32;
const KVM_RUN_IO_SIZE_OFFSET: usize = 33;
const KVM_RUN_IO_PORT_OFFSET: usize = 34;
const KVM_RUN_IO_COUNT_OFFSET: usize = 36;
const KVM_RUN_IO_DATA_OFFSET_OFFSET: usize = 40;
const KVM_RUN_IO_DATA_OFFSET: usize = 0x100;
const KVM_EXIT_IO_IN: u8 = 0;
const KVM_EXIT_IO_OUT: u8 = 1;
const KVM_RUN_MMIO_PHYS_ADDR_OFFSET: usize = 32;
const KVM_RUN_MMIO_DATA_OFFSET: usize = 40;
const KVM_RUN_MMIO_LEN_OFFSET: usize = 48;
const KVM_RUN_MMIO_IS_WRITE_OFFSET: usize = 52;
const KVM_EXIT_UNKNOWN: u32 = 0;
const KVM_EXIT_IO: u32 = 2;
const KVM_EXIT_HLT: u32 = 5;
const KVM_EXIT_MMIO: u32 = 6;
const KVM_EXIT_SHUTDOWN: u32 = 8;
const KVM_EXIT_FAIL_ENTRY: u32 = 9;
const KVM_EXIT_INTR: u32 = 10;
const KVM_EXIT_INTERNAL_ERROR: u32 = 17;
const KVM_EXIT_MEMORY_FAULT: u32 = 39;
const KVM_RUN_MAX_INTERNAL_EXITS: usize = 1024;
#[cfg(target_arch = "x86_64")]
const KVM_CPUID_FLAG_SIGNIFICANT_INDEX: u32 = 1;
#[cfg(target_arch = "riscv64")]
const RISCV_S_EXT_VECTOR: usize = (1usize << (usize::BITS - 1)) + 9;
const PAGE_SIZE: u64 = 4096;
const PAGE_SIZE_USIZE: usize = PAGE_SIZE as usize;
const X86_RAX_REG_INDEX: usize = 0;
const SUPPORTED_X86_MSRS: &[u32] = &[
    0x0000_0010, // IA32_TSC
    0x0000_0174, // IA32_SYSENTER_CS
    0x0000_0175, // IA32_SYSENTER_ESP
    0x0000_0176, // IA32_SYSENTER_EIP
    0x0000_01a0, // IA32_MISC_ENABLE
    0x0000_02ff, // MTRRdefType
    0xc000_0080, // EFER
    0xc000_0081, // STAR
    0xc000_0082, // LSTAR
    0xc000_0083, // CSTAR
    0xc000_0084, // SYSCALL_MASK
    0xc000_0100, // FS_BASE
    0xc000_0101, // GS_BASE
    0xc000_0102, // KERNEL_GS_BASE
];

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
    irqfds: BTreeMap<IrqFdKey, IrqFd>,
    gsi_routes: BTreeMap<u32, GsiRoute>,
    vcpu_files: BTreeMap<u32, api_control::ControlFileId>,
    tss_addr: Option<usize>,
    irqchip_created: bool,
    pit2_created: bool,
    gsi_routing_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VcpuFileState {
    vm_file: api_control::ControlFileId,
    vcpu_id: u32,
    mmap_area: api_control::MmapAreaId,
    mp_state: u32,
    pending_mmio_read: Option<PendingMmioRead>,
    pending_io_read: Option<PendingIoRead>,
    cpuid: Vec<KvmCpuidEntry2>,
    msrs: BTreeMap<u32, u64>,
    fpu: Vec<u8>,
    lapic: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingMmioRead {
    reg: usize,
    width: axaddrspace::device::AccessWidth,
    reg_width: axaddrspace::device::AccessWidth,
    signed_ext: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingIoRead {
    width: axaddrspace::device::AccessWidth,
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
    pio: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct IoEventFd {
    addr: u64,
    len: u32,
    datamatch: u64,
    user_fd_ref: api_control::UserFdRefId,
    flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct IrqFdKey {
    gsi: u32,
    fd: u32,
}

#[derive(Clone, Debug)]
struct IrqFd {
    user_fd_ref: api_control::UserFdRefId,
    cancel: Arc<AtomicBool>,
    task: TaskHandle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GsiRoute {
    IrqChip { pin: u32 },
    Msi { vector: u8 },
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct KvmCpuidEntry2 {
    function: u32,
    index: u32,
    flags: u32,
    eax: u32,
    ebx: u32,
    ecx: u32,
    edx: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KvmIrqFd {
    fd: u32,
    gsi: u32,
    flags: u32,
    resamplefd: u32,
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
            for irqfd in vm.irqfds.into_values() {
                stop_irqfd(irqfd);
            }
            for memory_slot in vm.memory_slots.into_values() {
                unmap_memory_slot(&vm.vm, memory_slot);
            }
            for vcpu_file in vm.vcpu_files.into_values() {
                if let Some(ControlFileState::Vcpu(vcpu)) = CONTROL_FILES.lock().remove(&vcpu_file)
                {
                    let _ = api_control::release_mmap_area(vcpu.mmap_area);
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
        KVM_GET_MSR_INDEX_LIST => get_msr_index_list(arg),
        KVM_CHECK_EXTENSION => Ok(check_extension(arg) as isize),
        KVM_GET_VCPU_MMAP_SIZE => Ok(KVM_VCPU_MMAP_SIZE as isize),
        KVM_GET_SUPPORTED_CPUID => get_supported_cpuid(arg),
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
        _ => {
            debug!("unsupported KVM system ioctl cmd={cmd:#x} arg={arg:#x}");
            ax_err!(Unsupported)
        }
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
            irqfds: BTreeMap::new(),
            gsi_routes: BTreeMap::new(),
            vcpu_files: BTreeMap::new(),
            tss_addr: None,
            irqchip_created: false,
            pit2_created: false,
            gsi_routing_count: 0,
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
        KVM_SET_TSS_ADDR => set_tss_addr(control_file, arg),
        KVM_CREATE_IRQCHIP => create_irqchip(control_file),
        KVM_CREATE_PIT2 => create_pit2(control_file, arg),
        KVM_SET_GSI_ROUTING => set_gsi_routing(control_file, arg),
        KVM_IRQFD => {
            let irqfd = read_irqfd(arg)?;
            update_irqfd(control_file, irqfd)?;
            Ok(0)
        }
        _ => {
            debug!("unsupported KVM VM ioctl cmd={cmd:#x} arg={arg:#x}");
            Err(AxError::Unsupported)
        }
    }
}

fn set_tss_addr(control_file: api_control::ControlFileId, addr: usize) -> AxResult<isize> {
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vm.tss_addr = Some(addr);
    Ok(0)
}

fn create_irqchip(control_file: api_control::ControlFileId) -> AxResult<isize> {
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vm.irqchip_created = true;
    Ok(0)
}

fn create_pit2(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let flags = read_u32_user(arg)?;
    if flags & !KVM_PIT_VALID_FLAGS != 0 {
        return ax_err!(InvalidInput);
    }

    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vm.pit2_created = true;
    Ok(0)
}

fn set_gsi_routing(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let routes = read_gsi_routes(arg)?;
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vm(vm)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vm.gsi_routing_count = routes.len() as u32;
    vm.gsi_routes = routes;
    Ok(0)
}

fn update_irqfd(control_file: api_control::ControlFileId, irqfd: KvmIrqFd) -> AxResult {
    validate_irqfd(irqfd)?;

    let key = IrqFdKey {
        gsi: irqfd.gsi,
        fd: irqfd.fd,
    };
    let user_fd_ref = if irqfd.flags & KVM_IRQFD_FLAG_DEASSIGN == 0 {
        Some(api_control::get_user_fd_ref(
            i32::try_from(irqfd.fd).map_err(|_| AxError::InvalidInput)?,
        )?)
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

    let old_irqfd = if irqfd.flags & KVM_IRQFD_FLAG_DEASSIGN != 0 {
        Some(vm.irqfds.remove(&key).ok_or(AxError::NotFound)?)
    } else {
        let old_irqfd = vm.irqfds.remove(&key);
        let user_fd_ref = user_fd_ref.unwrap();
        let (cancel, task) = start_irqfd_listener(control_file, irqfd.gsi, user_fd_ref);
        vm.irqfds.insert(
            key,
            IrqFd {
                user_fd_ref,
                cancel,
                task,
            },
        );
        old_irqfd
    };
    drop(control_files);

    if let Some(old_irqfd) = old_irqfd {
        stop_irqfd(old_irqfd);
    }
    Ok(())
}

fn validate_irqfd(irqfd: KvmIrqFd) -> AxResult {
    if irqfd.flags & !KVM_IRQFD_VALID_FLAGS != 0 {
        return ax_err!(InvalidInput);
    }
    if irqfd.flags & KVM_IRQFD_FLAG_RESAMPLE != 0 {
        return ax_err!(Unsupported);
    }
    if i32::try_from(irqfd.fd).is_err() {
        return ax_err!(InvalidInput);
    }
    Ok(())
}

fn vcpu_ioctl(control_file: api_control::ControlFileId, cmd: u32, arg: usize) -> AxResult<isize> {
    match cmd {
        KVM_RUN => run_vcpu_file(control_file),
        KVM_GET_REGS => get_kvm_regs(control_file, arg),
        KVM_SET_REGS => set_kvm_regs(control_file, arg),
        KVM_GET_SREGS => get_kvm_sregs(control_file, arg),
        KVM_SET_SREGS => set_kvm_sregs(control_file, arg),
        KVM_GET_MSRS => get_msrs(control_file, arg),
        KVM_SET_MSRS => set_msrs(control_file, arg),
        KVM_SET_FPU => set_fpu(control_file, arg),
        KVM_GET_LAPIC => get_lapic(control_file, arg),
        KVM_SET_LAPIC => set_lapic(control_file, arg),
        KVM_SET_CPUID2 => set_cpuid2(control_file, arg),
        KVM_GET_CPUID2 => get_cpuid2(control_file, arg),
        KVM_GET_ONE_REG => get_one_reg(control_file, arg),
        KVM_SET_ONE_REG => set_one_reg(control_file, arg),
        KVM_GET_REG_LIST => get_reg_list(control_file, arg),
        KVM_INTERRUPT => kvm_interrupt(control_file, arg),
        KVM_GET_MP_STATE => get_mp_state(control_file, arg),
        KVM_SET_MP_STATE => set_mp_state(control_file, arg),
        _ => {
            debug!("unsupported KVM vCPU ioctl cmd={cmd:#x} arg={arg:#x}");
            Err(AxError::Unsupported)
        }
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

fn get_msr_index_list(arg: usize) -> AxResult<isize> {
    let requested = read_u32_user(arg)? as usize;
    write_u32_user(arg, SUPPORTED_X86_MSRS.len() as u32)?;
    if requested < SUPPORTED_X86_MSRS.len() {
        return ax_err!(ArgumentListTooLong);
    }

    let mut offset = checked_add(arg, KVM_MSR_LIST_SIZE as usize)?;
    for msr in SUPPORTED_X86_MSRS {
        write_u32_user(offset, *msr)?;
        offset = checked_add(offset, 4)?;
    }
    Ok(0)
}

fn get_supported_cpuid(arg: usize) -> AxResult<isize> {
    let entries = supported_cpuid_entries();
    write_cpuid_entries(arg, &entries)
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
    let (vm, vcpu_id, vcpu, mp_state, pending_mmio_read, pending_io_read) = {
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
        )
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
    if let Some(pending) = pending_io_read {
        complete_io_read(control_file, &vcpu, pending)?;
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
                    false,
                )? =>
            {
                internal_exits = 0;
            }
            AxVCpuExitReason::IoWrite { port, width, data }
                if signal_matching_ioeventfd(
                    control_file,
                    port.number() as u64,
                    width,
                    data,
                    true,
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

fn set_cpuid2(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let entries = read_cpuid_entries(arg)?;
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vcpu.cpuid = entries;
    Ok(0)
}

fn get_cpuid2(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let entries = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        if vcpu.cpuid.is_empty() {
            supported_cpuid_entries()
        } else {
            vcpu.cpuid.clone()
        }
    };
    write_cpuid_entries(arg, &entries)
}

fn set_msrs(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let nmsrs = read_u32_user(arg)? as usize;
    if nmsrs > KVM_MAX_MSR_ENTRIES {
        return ax_err!(InvalidInput);
    }
    let entries_offset = checked_add(arg, KVM_MSRS_SIZE as usize)?;
    let mut entries = Vec::with_capacity(nmsrs);
    for index in 0..nmsrs {
        let offset = checked_add(entries_offset, index * KVM_MSR_ENTRY_SIZE)?;
        let msr_index = read_u32_user(offset)?;
        let data = read_u64_user(checked_add(offset, 8)?)?;
        entries.push((msr_index, data));
    }

    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    for (index, data) in entries {
        vcpu.msrs.insert(index, data);
    }
    Ok(nmsrs as isize)
}

fn get_msrs(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let nmsrs = read_u32_user(arg)? as usize;
    if nmsrs > KVM_MAX_MSR_ENTRIES {
        return ax_err!(InvalidInput);
    }
    let entries_offset = checked_add(arg, KVM_MSRS_SIZE as usize)?;
    let msrs = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        vcpu.msrs.clone()
    };

    for index in 0..nmsrs {
        let offset = checked_add(entries_offset, index * KVM_MSR_ENTRY_SIZE)?;
        let msr_index = read_u32_user(offset)?;
        let data = msrs
            .get(&msr_index)
            .copied()
            .unwrap_or_else(|| default_msr_value(msr_index));
        write_u64_user(checked_add(offset, 8)?, data)?;
    }
    Ok(nmsrs as isize)
}

fn set_fpu(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let mut bytes = vec![0u8; KVM_X86_FPU_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vcpu.fpu = bytes;
    Ok(0)
}

fn get_lapic(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let lapic = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vcpu(vcpu)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        vcpu.lapic.clone()
    };
    api_control::copy_to_user(arg, &lapic)?;
    Ok(0)
}

fn set_lapic(control_file: api_control::ControlFileId, arg: usize) -> AxResult<isize> {
    let mut bytes = vec![0u8; KVM_X86_LAPIC_STATE_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;
    let mut control_files = CONTROL_FILES.lock();
    let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
        return ax_err!(NotFound);
    };
    vcpu.lapic = bytes;
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
        AxVCpuExitReason::IoRead { .. } | AxVCpuExitReason::IoWrite { .. } => KVM_EXIT_IO,
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
        AxVCpuExitReason::IoRead { port, width } => {
            write_vcpu_run_u8(control_file, KVM_RUN_IO_DIRECTION_OFFSET, KVM_EXIT_IO_IN)?;
            write_vcpu_run_u8(
                control_file,
                KVM_RUN_IO_SIZE_OFFSET,
                access_width_bytes(*width) as u8,
            )?;
            write_vcpu_run_u16(control_file, KVM_RUN_IO_PORT_OFFSET, port.number())?;
            write_vcpu_run_u32(control_file, KVM_RUN_IO_COUNT_OFFSET, 1)?;
            write_vcpu_run_u64(
                control_file,
                KVM_RUN_IO_DATA_OFFSET_OFFSET,
                KVM_RUN_IO_DATA_OFFSET as u64,
            )?;

            let mut control_files = CONTROL_FILES.lock();
            let Some(ControlFileState::Vcpu(vcpu)) = control_files.get_mut(&control_file) else {
                return ax_err!(NotFound);
            };
            vcpu.pending_io_read = Some(PendingIoRead { width: *width });
        }
        AxVCpuExitReason::IoWrite { port, width, data } => {
            let mmap_area = control_file_mmap_area(control_file)?;
            write_vcpu_run_u8(control_file, KVM_RUN_IO_DIRECTION_OFFSET, KVM_EXIT_IO_OUT)?;
            write_vcpu_run_u8(
                control_file,
                KVM_RUN_IO_SIZE_OFFSET,
                access_width_bytes(*width) as u8,
            )?;
            write_vcpu_run_u16(control_file, KVM_RUN_IO_PORT_OFFSET, port.number())?;
            write_vcpu_run_u32(control_file, KVM_RUN_IO_COUNT_OFFSET, 1)?;
            write_vcpu_run_u64(
                control_file,
                KVM_RUN_IO_DATA_OFFSET_OFFSET,
                KVM_RUN_IO_DATA_OFFSET as u64,
            )?;
            api_control::write_mmap_area(
                mmap_area,
                KVM_RUN_IO_DATA_OFFSET,
                &data.to_ne_bytes()[..access_width_bytes(*width) as usize],
            )?;
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

fn complete_io_read(
    control_file: api_control::ControlFileId,
    vcpu: &axvm::AxVCpuRef,
    pending: PendingIoRead,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    let mut bytes = [0u8; 8];
    let len = access_width_bytes(pending.width) as usize;
    api_control::read_mmap_area(mmap_area, KVM_RUN_IO_DATA_OFFSET, &mut bytes[..len])?;
    let value = u64::from_ne_bytes(bytes) as usize & access_width_mask(pending.width);
    vcpu.set_gpr(X86_RAX_REG_INDEX, value);
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
        KVM_CAP_IRQCHIP => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_USER_MEMORY => 1,
        KVM_CAP_SET_TSS_ADDR => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_EXT_CPUID => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_IOEVENTFD => 1,
        KVM_CAP_NR_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_MAX_VCPUS => KVM_MAX_VCPUS,
        KVM_CAP_NR_MEMSLOTS => KVM_MAX_MEMORY_SLOTS,
        KVM_CAP_MP_STATE => 1,
        KVM_CAP_IRQFD => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_PIT2 => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_PIT_STATE2 => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_ADJUST_CLOCK => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_DEBUGREGS => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_VCPU_EVENTS => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_XCRS => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_XSAVE => usize::from(cfg!(target_arch = "x86_64")),
        KVM_CAP_ONE_REG => usize::from(cfg!(target_arch = "riscv64")),
        KVM_CAP_IMMEDIATE_EXIT => 1,
        KVM_CAP_XSAVE2 => 0,
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
                pending_io_read: None,
                cpuid: Vec::new(),
                msrs: BTreeMap::new(),
                fpu: default_fpu(),
                lapic: vec![0; KVM_X86_LAPIC_STATE_SIZE as usize],
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

fn read_irqfd(arg: usize) -> AxResult<KvmIrqFd> {
    let mut bytes = [0u8; KVM_IRQFD_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;

    Ok(KvmIrqFd {
        fd: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
        gsi: u32::from_ne_bytes(bytes[4..8].try_into().unwrap()),
        flags: u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
        resamplefd: u32::from_ne_bytes(bytes[12..16].try_into().unwrap()),
    })
}

fn read_gsi_routes(arg: usize) -> AxResult<BTreeMap<u32, GsiRoute>> {
    let route_count = read_u32_user(arg)? as usize;
    if route_count > KVM_MAX_IRQ_ROUTES {
        return ax_err!(InvalidInput);
    }

    let mut routes = BTreeMap::new();
    let mut offset = checked_add(arg, KVM_IRQ_ROUTING_SIZE as usize)?;
    for _ in 0..route_count {
        let gsi = read_u32_user(offset)?;
        let route_type = read_u32_user(checked_add(offset, 4)?)?;
        let flags = read_u32_user(checked_add(offset, 8)?)?;
        if flags != 0 {
            return ax_err!(Unsupported);
        }

        let route = match route_type {
            KVM_IRQ_ROUTING_IRQCHIP => {
                let pin = read_u32_user(checked_add(offset, 20)?)?;
                GsiRoute::IrqChip { pin }
            }
            KVM_IRQ_ROUTING_MSI => {
                let data = read_u32_user(checked_add(offset, 24)?)?;
                GsiRoute::Msi {
                    vector: (data & 0xff) as u8,
                }
            }
            _ => return ax_err!(Unsupported),
        };
        routes.insert(gsi, route);
        offset = checked_add(offset, KVM_IRQ_ROUTING_ENTRY_SIZE)?;
    }

    Ok(routes)
}

fn start_irqfd_listener(
    vm_file: api_control::ControlFileId,
    gsi: u32,
    user_fd_ref: api_control::UserFdRefId,
) -> (Arc<AtomicBool>, TaskHandle) {
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_task = cancel.clone();
    let task = api_task::spawn_task(
        TaskOptions {
            name: format!("kvm-irqfd-{vm_file}-{gsi}"),
            stack_size: 64 * 1024,
            cpu_set: None,
        },
        move || irqfd_listener_loop(vm_file, gsi, user_fd_ref, cancel_for_task),
    );
    (cancel, task)
}

fn stop_irqfd(irqfd: IrqFd) {
    irqfd.cancel.store(true, Ordering::Release);
    api_task::join_task(irqfd.task);
    let _ = api_control::release_user_fd_ref(irqfd.user_fd_ref);
}

fn irqfd_listener_loop(
    vm_file: api_control::ControlFileId,
    gsi: u32,
    user_fd_ref: api_control::UserFdRefId,
    cancel: Arc<AtomicBool>,
) {
    while !cancel.load(Ordering::Acquire) {
        let mut bytes = [0u8; 8];
        match api_control::read_user_fd_ref(user_fd_ref, &mut bytes) {
            Ok(read_len) if read_len == core::mem::size_of::<u64>() => {
                if u64::from_ne_bytes(bytes) != 0
                    && let Err(err) = inject_irqfd_gsi(vm_file, gsi)
                {
                    warn!("KVM irqfd injection failed for GSI {gsi}: {err:?}");
                }
            }
            Ok(_) => axvisor_api::task::yield_now(),
            Err(err)
                if matches!(
                    AxErrorKind::try_from(err),
                    Ok(AxErrorKind::WouldBlock | AxErrorKind::Interrupted)
                ) =>
            {
                axvisor_api::task::yield_now();
            }
            Err(err) => {
                debug!("KVM irqfd listener exiting for GSI {gsi}: {err:?}");
                break;
            }
        }
    }
}

fn inject_irqfd_gsi(control_file: api_control::ControlFileId, gsi: u32) -> AxResult {
    let (vm, vector) = {
        let control_files = CONTROL_FILES.lock();
        let Some(ControlFileState::Vm(vm)) = control_files.get(&control_file) else {
            return ax_err!(NotFound);
        };
        let vector = vm
            .gsi_routes
            .get(&gsi)
            .map(gsi_route_vector)
            .unwrap_or_else(|| legacy_gsi_vector(gsi));
        (vm.vm.clone(), vector)
    };

    vm.vcpu(0)
        .ok_or(AxError::InvalidInput)?
        .inject_interrupt(vector as usize)
}

fn gsi_route_vector(route: &GsiRoute) -> u8 {
    match *route {
        GsiRoute::IrqChip { pin } => legacy_gsi_vector(pin),
        GsiRoute::Msi { vector } => vector,
    }
}

fn legacy_gsi_vector(gsi: u32) -> u8 {
    0x20u8.saturating_add(gsi.min(0xdf) as u8)
}

fn update_ioeventfd(control_file: api_control::ControlFileId, ioeventfd: KvmIoEventFd) -> AxResult {
    validate_ioeventfd(ioeventfd)?;

    let key = IoEventFdKey {
        addr: ioeventfd.addr,
        datamatch: ioeventfd.datamatch,
        pio: ioeventfd.flags & KVM_IOEVENTFD_FLAG_PIO != 0,
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
    pio: bool,
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
            .find(|ioeventfd| ioeventfd_matches(ioeventfd, addr, width, data, pio))
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

fn ioeventfd_matches(
    ioeventfd: &IoEventFd,
    addr: u64,
    width: AccessWidth,
    data: u64,
    pio: bool,
) -> bool {
    if (ioeventfd.flags & KVM_IOEVENTFD_FLAG_PIO != 0) != pio {
        return false;
    }
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

fn write_vcpu_run_u16(
    control_file: api_control::ControlFileId,
    offset: usize,
    value: u16,
) -> AxResult {
    let mmap_area = control_file_mmap_area(control_file)?;
    api_control::write_mmap_area(mmap_area, offset, &value.to_ne_bytes())
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

fn checked_add(base: usize, offset: usize) -> AxResult<usize> {
    base.checked_add(offset).ok_or(AxError::InvalidInput)
}

fn read_one_reg(arg: usize) -> AxResult<OneReg> {
    let mut bytes = [0u8; KVM_ONE_REG_SIZE as usize];
    api_control::copy_from_user(arg, &mut bytes)?;

    Ok(OneReg {
        id: u64::from_ne_bytes(bytes[0..8].try_into().unwrap()),
        addr: u64::from_ne_bytes(bytes[8..16].try_into().unwrap()),
    })
}

fn read_cpuid_entries(arg: usize) -> AxResult<Vec<KvmCpuidEntry2>> {
    let nent = read_u32_user(arg)? as usize;
    if nent > KVM_MAX_CPUID_ENTRIES {
        return ax_err!(InvalidInput);
    }
    let entries_offset = checked_add(arg, KVM_CPUID2_SIZE as usize)?;
    let mut entries = Vec::with_capacity(nent);
    for index in 0..nent {
        let offset = checked_add(entries_offset, index * KVM_CPUID_ENTRY2_SIZE)?;
        entries.push(read_cpuid_entry(offset)?);
    }
    Ok(entries)
}

fn write_cpuid_entries(arg: usize, entries: &[KvmCpuidEntry2]) -> AxResult<isize> {
    let requested = read_u32_user(arg)? as usize;
    write_u32_user(arg, entries.len() as u32)?;
    if requested < entries.len() {
        return ax_err!(ArgumentListTooLong);
    }

    let mut offset = checked_add(arg, KVM_CPUID2_SIZE as usize)?;
    for entry in entries {
        api_control::copy_to_user(offset, &entry.to_bytes())?;
        offset = checked_add(offset, KVM_CPUID_ENTRY2_SIZE)?;
    }
    Ok(0)
}

fn read_cpuid_entry(arg: usize) -> AxResult<KvmCpuidEntry2> {
    let mut bytes = [0u8; KVM_CPUID_ENTRY2_SIZE];
    api_control::copy_from_user(arg, &mut bytes)?;

    Ok(KvmCpuidEntry2 {
        function: u32::from_ne_bytes(bytes[0..4].try_into().unwrap()),
        index: u32::from_ne_bytes(bytes[4..8].try_into().unwrap()),
        flags: u32::from_ne_bytes(bytes[8..12].try_into().unwrap()),
        eax: u32::from_ne_bytes(bytes[12..16].try_into().unwrap()),
        ebx: u32::from_ne_bytes(bytes[16..20].try_into().unwrap()),
        ecx: u32::from_ne_bytes(bytes[20..24].try_into().unwrap()),
        edx: u32::from_ne_bytes(bytes[24..28].try_into().unwrap()),
    })
}

impl KvmCpuidEntry2 {
    fn to_bytes(self) -> [u8; KVM_CPUID_ENTRY2_SIZE] {
        let mut bytes = [0u8; KVM_CPUID_ENTRY2_SIZE];
        bytes[0..4].copy_from_slice(&self.function.to_ne_bytes());
        bytes[4..8].copy_from_slice(&self.index.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.flags.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.eax.to_ne_bytes());
        bytes[16..20].copy_from_slice(&self.ebx.to_ne_bytes());
        bytes[20..24].copy_from_slice(&self.ecx.to_ne_bytes());
        bytes[24..28].copy_from_slice(&self.edx.to_ne_bytes());
        bytes
    }
}

fn supported_cpuid_entries() -> Vec<KvmCpuidEntry2> {
    #[cfg(target_arch = "x86_64")]
    {
        supported_cpuid_entries_x86_64()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Vec::new()
    }
}

#[cfg(target_arch = "x86_64")]
fn supported_cpuid_entries_x86_64() -> Vec<KvmCpuidEntry2> {
    let max_basic = host_cpuid(0, 0).eax;
    let mut entries = Vec::new();

    push_host_cpuid(&mut entries, 0, 0, 0);
    if max_basic >= 1 {
        push_host_cpuid(&mut entries, 1, 0, 0);
    }
    if max_basic >= 4 {
        for index in 0..=16 {
            let entry = host_cpuid(4, index);
            if entry.eax & 0x1f == 0 {
                break;
            }
            entries.push(KvmCpuidEntry2 {
                function: 4,
                index,
                flags: KVM_CPUID_FLAG_SIGNIFICANT_INDEX,
                ..entry
            });
        }
    }
    if max_basic >= 6 {
        push_host_cpuid(&mut entries, 6, 0, 0);
    }
    if max_basic >= 7 {
        let max_subleaf = host_cpuid(7, 0).eax.min(2);
        for index in 0..=max_subleaf {
            push_host_cpuid(&mut entries, 7, index, KVM_CPUID_FLAG_SIGNIFICANT_INDEX);
        }
    }
    if max_basic >= 0xa {
        push_host_cpuid(&mut entries, 0xa, 0, 0);
    }
    if max_basic >= 0xb {
        for index in 0..=8 {
            let entry = host_cpuid(0xb, index);
            if index != 0 && entry.ebx == 0 {
                break;
            }
            entries.push(KvmCpuidEntry2 {
                function: 0xb,
                index,
                flags: KVM_CPUID_FLAG_SIGNIFICANT_INDEX,
                ..entry
            });
        }
    }
    if max_basic >= 0xd {
        push_host_cpuid(&mut entries, 0xd, 0, KVM_CPUID_FLAG_SIGNIFICANT_INDEX);
        push_host_cpuid(&mut entries, 0xd, 1, KVM_CPUID_FLAG_SIGNIFICANT_INDEX);
    }
    if max_basic >= 0x15 {
        push_host_cpuid(&mut entries, 0x15, 0, 0);
    }
    if max_basic >= 0x16 {
        push_host_cpuid(&mut entries, 0x16, 0, 0);
    }
    if max_basic >= 0x1f {
        for index in 0..=8 {
            let entry = host_cpuid(0x1f, index);
            if index != 0 && entry.ebx == 0 {
                break;
            }
            entries.push(KvmCpuidEntry2 {
                function: 0x1f,
                index,
                flags: KVM_CPUID_FLAG_SIGNIFICANT_INDEX,
                ..entry
            });
        }
    }

    let max_extended = host_cpuid(0x8000_0000, 0).eax;
    push_host_cpuid(&mut entries, 0x8000_0000, 0, 0);
    for function in 0x8000_0001..=max_extended.min(0x8000_0008) {
        push_host_cpuid(&mut entries, function, 0, 0);
    }

    entries
}

#[cfg(target_arch = "x86_64")]
fn push_host_cpuid(entries: &mut Vec<KvmCpuidEntry2>, function: u32, index: u32, flags: u32) {
    let mut entry = host_cpuid(function, index);
    entry.flags = flags;
    entries.push(entry);
}

#[cfg(target_arch = "x86_64")]
fn host_cpuid(function: u32, index: u32) -> KvmCpuidEntry2 {
    let result = core::arch::x86_64::__cpuid_count(function, index);
    let mut entry = KvmCpuidEntry2 {
        function,
        index,
        flags: 0,
        eax: result.eax,
        ebx: result.ebx,
        ecx: result.ecx,
        edx: result.edx,
    };

    match function {
        1 => {
            entry.ecx |= 1 << 31; // hypervisor present
            entry.ecx &= !(1 << 5); // VMX
        }
        0x8000_0001 => {
            entry.ecx &= !(1 << 2); // SVM
        }
        _ => {}
    }
    entry
}

fn default_msr_value(msr: u32) -> u64 {
    match msr {
        0x0000_01a0 => 1,               // IA32_MISC_ENABLE fast string
        0x0000_02ff => (1 << 11) | 0x6, // MTRR enabled, write-back default type
        _ => 0,
    }
}

fn default_fpu() -> Vec<u8> {
    let mut fpu = vec![0; KVM_X86_FPU_SIZE as usize];
    fpu[128..130].copy_from_slice(&0x37fu16.to_ne_bytes());
    fpu[408..412].copy_from_slice(&0x1f80u32.to_ne_bytes());
    fpu
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
