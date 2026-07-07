//! Architecture component glue owned by AxVM.

use alloc::vec::Vec;

use ax_errno::AxResult;
#[cfg(not(target_arch = "aarch64"))]
use ax_errno::ax_err;
use ax_memory_addr::{PhysAddr, VirtAddr};
use axaddrspace::NestedPageTableOps;
use axvm_types::{
    AccessWidth, GuestPhysAddr, NestedPagingConfig, PassThroughPortConfig, SysRegAddr,
    VMInterruptMode, VmArchPerCpuOps, VmArchVcpuOps,
};
#[cfg(not(target_arch = "aarch64"))]
use axvm_types::{MappingFlags, Port, VmExit};

use crate::{CpuMask, StopReason};

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "loongarch64")]
mod loongarch64;
mod npt;
#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub(crate) type CurrentArch = aarch64::Aarch64Arch;
#[cfg(target_arch = "loongarch64")]
pub(crate) type CurrentArch = loongarch64::LoongArch64Arch;
#[cfg(target_arch = "riscv64")]
pub(crate) type CurrentArch = riscv64::Riscv64Arch;
#[cfg(target_arch = "x86_64")]
pub(crate) type CurrentArch = x86_64::X86_64Arch;

pub(crate) type ArchVCpu = <CurrentArch as ArchOps>::VCpu;
pub(crate) type ArchPerCpu = <CurrentArch as ArchOps>::PerCpu;
pub(crate) type ArchNestedPageTable = <CurrentArch as ArchOps>::NestedPageTable;

/// Result of handling one architecture-specific vCPU exit.
#[derive(Debug)]
pub(crate) enum VcpuRunAction {
    /// The exit was handled completely; re-enter the guest in the current run slice.
    Continue,
    /// Handle a host external interrupt after the vCPU has been unbound.
    HostInterrupt(usize),
    /// Return to the runtime loop without blocking.
    Yield,
    /// Block the current vCPU task on the VM runtime wait queue.
    Wait,
    /// Request VM stop with the provided reason.
    Stop(StopReason),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioReadExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) width: AccessWidth,
    pub(crate) reg: usize,
    pub(crate) reg_width: AccessWidth,
    pub(crate) signed_ext: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioWriteExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) width: AccessWidth,
    pub(crate) data: u64,
}

#[derive(Clone, Copy, Debug)]
#[cfg(not(target_arch = "aarch64"))]
pub(crate) struct IoReadExit {
    pub(crate) port: Port,
    pub(crate) width: AccessWidth,
}

#[derive(Clone, Copy, Debug)]
#[cfg(not(target_arch = "aarch64"))]
pub(crate) struct IoWriteExit {
    pub(crate) port: Port,
    pub(crate) width: AccessWidth,
    pub(crate) data: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SysRegReadExit {
    pub(crate) addr: SysRegAddr,
    pub(crate) reg: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SysRegWriteExit {
    pub(crate) addr: SysRegAddr,
    pub(crate) value: u64,
}

#[derive(Clone, Copy, Debug)]
#[cfg(not(target_arch = "aarch64"))]
pub(crate) struct NestedPageFaultExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) access_flags: MappingFlags,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HypercallExit {
    pub(crate) nr: u64,
    pub(crate) args: [u64; 6],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuUpExit {
    pub(crate) target_cpu: u64,
    pub(crate) entry_point: GuestPhysAddr,
    pub(crate) arg: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SendIpiExit {
    pub(crate) target_cpu: u64,
    pub(crate) target_cpu_aux: u64,
    pub(crate) send_to_all: bool,
    pub(crate) send_to_self: bool,
    pub(crate) vector: u64,
}

#[allow(dead_code)]
pub(crate) struct VcpuCreateContext {
    pub(crate) vcpu_id: usize,
    pub(crate) phys_cpu_id: usize,
    pub(crate) dtb_addr: Option<GuestPhysAddr>,
    pub(crate) firmware_boot: bool,
}

#[allow(dead_code)]
pub(crate) struct VcpuSetupContext<'a> {
    pub(crate) interrupt_mode: VMInterruptMode,
    pub(crate) emulates_console: bool,
    pub(crate) passthrough_ports: &'a [PassThroughPortConfig],
    pub(crate) firmware_boot: bool,
}

pub(crate) trait ArchOps {
    type VCpu: VmArchVcpuOps;
    type PerCpu: VmArchPerCpuOps;
    type VcpuCreateState;
    type NestedPageTable: NestedPageTableOps;

    fn has_hardware_support() -> bool;

    fn max_guest_page_table_levels() -> usize {
        4
    }

    fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxResult<usize> {
        let mut levels = Self::max_guest_page_table_levels();
        for cpu_id in target_phys_cpu_ids(vcpu_mappings) {
            levels = levels.min(
                crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
                    .unwrap_or_else(Self::max_guest_page_table_levels),
            );
        }
        Ok(levels)
    }

    fn nested_paging_config(
        root_paddr: PhysAddr,
        levels: usize,
        _vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<NestedPagingConfig> {
        let gpa_bits = match levels {
            3 => 39,
            4 => 48,
            _ => return ax_errno::ax_err!(InvalidInput, "unsupported nested page-table levels"),
        };
        Ok(NestedPagingConfig::new(root_paddr, levels, gpa_bits, 0))
    }

    fn new_nested_page_table(levels: usize) -> AxResult<Self::NestedPageTable>;

    fn clean_dcache_range(_addr: VirtAddr, _size: usize) {}

    fn new_vcpu_create_state(
        vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<Self::VcpuCreateState>;

    fn build_vcpu_create_config(
        state: &Self::VcpuCreateState,
        ctx: VcpuCreateContext,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::CreateConfig>;

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::SetupConfig>;

    fn register_platform_irq_injector() {}

    fn vcpu_affinities(
        cpu_num: usize,
        phys_cpu_ids: Option<&[usize]>,
        phys_cpu_sets: Option<&[usize]>,
    ) -> Vec<(usize, Option<usize>, usize)> {
        default_vcpu_affinities(cpu_num, phys_cpu_ids, phys_cpu_sets)
    }

    fn ipi_targets(
        vm: &crate::AxVMRef,
        current_vcpu_id: usize,
        target_cpu: u64,
        target_cpu_aux: u64,
        send_to_all: bool,
        send_to_self: bool,
    ) -> CpuMask<64> {
        let mut targets = CpuMask::new();

        if send_to_all {
            for vcpu in vm.vcpu_list() {
                if vcpu.id() != current_vcpu_id {
                    targets.set(vcpu.id(), true);
                }
            }
        } else if send_to_self {
            targets.set(current_vcpu_id, true);
        } else {
            let _ = target_cpu_aux;
            targets.set(target_cpu as usize, true);
        }

        targets
    }

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef, _vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(0, arg);
    }

    fn set_cpu_up_success(vcpu: &crate::vm::AxVCpuRef) {
        vcpu.set_gpr(0, 0);
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn set_io_read_result(vcpu: &crate::vm::AxVCpuRef, val: usize) {
        vcpu.set_gpr(0, val);
    }

    fn before_first_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {}

    fn before_vcpu_run(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {}

    fn inject_pending_interrupt(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef,
        interrupt: crate::vm::PendingInterrupt,
    ) {
        match interrupt {
            crate::vm::PendingInterrupt::Normal(vector) => {
                trace!(
                    "Injecting queued interrupt {vector:#x} into VM[{}] VCpu[{}]",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                if let Err(err) = vcpu.inject_interrupt(vector) {
                    warn!(
                        "Failed to inject queued interrupt {vector:#x} into VM[{}] VCpu[{}]: \
                         {err:?}",
                        vcpu.vm_id(),
                        vcpu.id()
                    );
                }
            }
            crate::vm::PendingInterrupt::External {
                vector,
                physical_irq,
            } => {
                warn!(
                    "VM[{}] VCpu[{}] dropped unsupported external interrupt vector={vector:#x}, \
                     physical_irq={physical_irq:#x}",
                    vcpu.vm_id(),
                    vcpu.id()
                );
            }
        }
    }

    fn after_external_interrupt(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef, vector: usize) {
        crate::host::arceos::dispatch_host_irq(vector);
        crate::check_timer_events();
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn after_preemption_timer(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {
        crate::check_timer_events();
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn after_interrupt_end(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef,
        _vector: Option<u8>,
    ) {
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn handle_idle(_vm: &crate::AxVMRef, _vcpu: &crate::vm::AxVCpuRef) {
        crate::check_timer_events();
    }

    fn on_last_vcpu_exit(_vm_id: usize) {}

    fn after_mmio_write(_vm: &crate::AxVM) {}

    fn cpu_up_target_vcpu_id(vm: &crate::AxVMRef, target_cpu: u64) -> Option<usize> {
        vm.get_vcpu_affinities_pcpu_ids()
            .iter()
            .find_map(|(vcpu_id, _, phys_id)| (*phys_id == target_cpu as usize).then_some(*vcpu_id))
    }

    #[cfg(not(target_arch = "aarch64"))]
    fn handle_halt() -> VcpuRunAction {
        VcpuRunAction::Wait
    }

    fn handle_vcpu_exit(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxResult<VcpuRunAction>;
}

pub(crate) fn handle_mmio_read(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef,
    exit: MmioReadExit,
) -> AxResult<VcpuRunAction> {
    let raw = vm.get_devices()?.handle_mmio_read(exit.addr, exit.width)?;
    let masked = raw & crate::vm::width_mask(exit.width);
    let val = if exit.signed_ext {
        crate::vm::sign_extend_value(masked, exit.width)
    } else {
        masked & crate::vm::width_mask(exit.reg_width)
    };
    vcpu.set_gpr(exit.reg, val);
    Ok(VcpuRunAction::Continue)
}

pub(crate) fn handle_mmio_write(vm: &crate::AxVM, exit: MmioWriteExit) -> AxResult<VcpuRunAction> {
    vm.handle_mmio_write(exit.addr, exit.width, exit.data as usize)?;
    Ok(VcpuRunAction::Continue)
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn handle_io_read<A: ArchOps>(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef,
    exit: IoReadExit,
) -> AxResult<VcpuRunAction> {
    let val = vm.get_devices()?.handle_port_read(exit.port, exit.width)?;
    A::set_io_read_result(vcpu, val);
    Ok(VcpuRunAction::Continue)
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn handle_io_write(vm: &crate::AxVM, exit: IoWriteExit) -> AxResult<VcpuRunAction> {
    vm.get_devices()?
        .handle_port_write(exit.port, exit.width, exit.data as usize)?;
    Ok(VcpuRunAction::Continue)
}

pub(crate) fn handle_sys_reg_read(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef,
    exit: SysRegReadExit,
) -> AxResult<VcpuRunAction> {
    let val = vm.get_devices()?.handle_sys_reg_read(
        exit.addr,
        // System registers are currently modeled as fixed-width device registers.
        AccessWidth::Qword,
    )?;
    vcpu.set_gpr(exit.reg, val);
    Ok(VcpuRunAction::Continue)
}

pub(crate) fn handle_sys_reg_write(
    vm: &crate::AxVM,
    exit: SysRegWriteExit,
) -> AxResult<VcpuRunAction> {
    vm.get_devices()?
        .handle_sys_reg_write(exit.addr, AccessWidth::Qword, exit.value as usize)?;
    Ok(VcpuRunAction::Continue)
}

#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn handle_nested_page_fault<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef,
    exit: NestedPageFaultExit,
) -> AxResult<VcpuRunAction> {
    if vm.get_devices()?.find_mmio_dev(exit.addr).is_some() {
        let Some(decoded) = vcpu
            .get_arch_vcpu()
            .decode_mmio_fault(exit.addr, exit.access_flags)
        else {
            warn!(
                "VM[{}] VCpu[{}] nested page fault at {:#x} maps MMIO but cannot be decoded",
                vm.id(),
                vcpu.id(),
                exit.addr.as_usize()
            );
            return Ok(VcpuRunAction::Yield);
        };
        return handle_transitional_vm_exit::<A>(vm, vcpu, decoded);
    }

    if vm.handle_nested_page_fault(exit.addr, exit.access_flags) {
        Ok(VcpuRunAction::Continue)
    } else {
        warn!(
            "VM[{}] VCpu[{}] unhandled nested page fault at {:#x}, access={:?}",
            vm.id(),
            vcpu.id(),
            exit.addr.as_usize(),
            exit.access_flags
        );
        Ok(VcpuRunAction::Yield)
    }
}

pub(crate) fn handle_hypercall(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef,
    exit: HypercallExit,
) -> AxResult<VcpuRunAction> {
    debug!("Hypercall [{:#x}] args {:x?}", exit.nr, exit.args);
    match crate::runtime::hvc::HyperCall::new(vm.clone(), exit.nr, exit.args) {
        Ok(hypercall) => {
            let ret_val = match hypercall.execute() {
                Ok(ret_val) => ret_val as isize,
                Err(err) => {
                    warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
                    -1
                }
            };
            vcpu.set_return_value(ret_val as usize);
        }
        Err(err) => {
            warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
        }
    }
    Ok(VcpuRunAction::Yield)
}

pub(crate) fn handle_cpu_up<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef,
    exit: CpuUpExit,
) -> AxResult<VcpuRunAction> {
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();
    info!(
        "VM[{vm_id}]'s VCpu[{vcpu_id}] try to boot target_cpu [{}] entry_point={:x} arg={:#x}",
        exit.target_cpu, exit.entry_point, exit.arg
    );

    let Some(target_vcpu_id) = A::cpu_up_target_vcpu_id(vm, exit.target_cpu) else {
        warn!(
            "VM[{vm_id}] cannot resolve architecture CPU target {} to a VM-local vCPU",
            exit.target_cpu
        );
        vcpu.set_return_value(usize::MAX);
        return Ok(VcpuRunAction::Yield);
    };

    match crate::runtime::vcpus::vcpu_on(
        vm.clone(),
        target_vcpu_id,
        exit.entry_point,
        exit.arg as _,
    ) {
        Ok(()) => A::set_cpu_up_success(vcpu),
        Err(err) => {
            warn!("Failed to boot VM[{vm_id}] VCpu[{target_vcpu_id}]: {err:?}");
            vcpu.set_return_value(usize::MAX);
        }
    }
    Ok(VcpuRunAction::Yield)
}

pub(crate) fn handle_send_ipi<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu_id: usize,
    exit: SendIpiExit,
) -> AxResult<VcpuRunAction> {
    let vm_id = vm.id();
    debug!(
        "VM[{vm_id}] run VCpu[{vcpu_id}] SendIPI, target_cpu={:#x}, target_cpu_aux={:#x}, \
         vector={}",
        exit.target_cpu, exit.target_cpu_aux, exit.vector
    );
    let targets = A::ipi_targets(
        vm,
        vcpu_id,
        exit.target_cpu,
        exit.target_cpu_aux,
        exit.send_to_all,
        exit.send_to_self,
    );
    if targets.is_empty() {
        warn!(
            "VM[{vm_id}] SendIPI has no target: target_cpu={:#x}, target_cpu_aux={:#x}",
            exit.target_cpu, exit.target_cpu_aux
        );
        return Ok(VcpuRunAction::Yield);
    }

    if targets.get(vcpu_id) {
        crate::inject_current_vcpu_interrupt(exit.vector as _)
            .expect("failed to inject self IPI into current vCPU");
    }
    let mut remote_targets = targets;
    remote_targets.set(vcpu_id, false);
    if !remote_targets.is_empty()
        && let Err(err) = vm.inject_interrupt_to_vcpu(remote_targets, exit.vector as _)
    {
        warn!(
            "Failed to inject interrupt {} to VM[{vm_id}] targets {remote_targets:?}: {err:?}",
            exit.vector
        );
    }
    Ok(VcpuRunAction::Yield)
}

/// Transitional handler for architecture backends that still return the legacy
/// common `VmExit` while their raw exit enums are being split out.
#[cfg(not(target_arch = "aarch64"))]
pub(crate) fn handle_transitional_vm_exit<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef,
    exit: VmExit,
) -> AxResult<VcpuRunAction> {
    match exit {
        VmExit::Hypercall { nr, args } => handle_hypercall(vm, vcpu, HypercallExit { nr, args }),
        VmExit::MmioRead {
            addr,
            width,
            reg,
            reg_width,
            signed_ext,
        } => handle_mmio_read(
            vm,
            vcpu,
            MmioReadExit {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            },
        ),
        VmExit::MmioWrite { addr, width, data } => {
            handle_mmio_write(vm, MmioWriteExit { addr, width, data })
        }
        VmExit::IoRead { port, width } => handle_io_read::<A>(vm, vcpu, IoReadExit { port, width }),
        VmExit::IoWrite { port, width, data } => {
            handle_io_write(vm, IoWriteExit { port, width, data })
        }
        VmExit::SysRegRead { addr, reg } => {
            handle_sys_reg_read(vm, vcpu, SysRegReadExit { addr, reg })
        }
        VmExit::SysRegWrite { addr, value } => {
            handle_sys_reg_write(vm, SysRegWriteExit { addr, value })
        }
        VmExit::NestedPageFault { addr, access_flags } => {
            handle_nested_page_fault::<A>(vm, vcpu, NestedPageFaultExit { addr, access_flags })
        }
        VmExit::ExternalInterrupt { vector } => {
            debug!("VM[{}] run VCpu[{}] get irq {vector}", vm.id(), vcpu.id());
            Ok(VcpuRunAction::HostInterrupt(vector as usize))
        }
        VmExit::PreemptionTimer => {
            A::after_preemption_timer(vm, vcpu);
            Ok(VcpuRunAction::Yield)
        }
        VmExit::InterruptEnd { vector } => {
            A::after_interrupt_end(vm, vcpu, vector);
            Ok(VcpuRunAction::Yield)
        }
        VmExit::Halt => {
            debug!("VM[{}] run VCpu[{}] Halt", vm.id(), vcpu.id());
            Ok(A::handle_halt())
        }
        VmExit::Idle => {
            trace!("VM[{}] run VCpu[{}] Idle", vm.id(), vcpu.id());
            A::handle_idle(vm, vcpu);
            Ok(VcpuRunAction::Yield)
        }
        VmExit::Nothing => Ok(VcpuRunAction::Yield),
        VmExit::CpuDown { _state } => {
            warn!(
                "VM[{}] run VCpu[{}] CpuDown state {_state:#x}",
                vm.id(),
                vcpu.id()
            );
            Ok(VcpuRunAction::Wait)
        }
        VmExit::CpuUp {
            target_cpu,
            entry_point,
            arg,
        } => handle_cpu_up::<A>(
            vm,
            vcpu,
            CpuUpExit {
                target_cpu,
                entry_point,
                arg,
            },
        ),
        VmExit::SystemDown => {
            warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
            Ok(VcpuRunAction::Stop(StopReason::SystemDown))
        }
        VmExit::FailEntry {
            hardware_entry_failure_reason,
        } => {
            warn!(
                "VM[{}] VCpu[{}] run failed with exit code {hardware_entry_failure_reason}",
                vm.id(),
                vcpu.id()
            );
            Ok(VcpuRunAction::Yield)
        }
        VmExit::SendIPI {
            target_cpu,
            target_cpu_aux,
            send_to_all,
            send_to_self,
            vector,
        } => handle_send_ipi::<A>(
            vm,
            vcpu.id(),
            SendIpiExit {
                target_cpu,
                target_cpu_aux,
                send_to_all,
                send_to_self,
                vector,
            },
        ),
        _ => ax_err!(Unsupported, "unsupported legacy VM exit"),
    }
}

pub(crate) fn target_phys_cpu_ids(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> Vec<usize> {
    let mut cpu_ids = Vec::new();
    for (_, maybe_mask, phys_id) in vcpu_mappings {
        if let Some(mask) = maybe_mask {
            for cpu_id in 0..usize::BITS as usize {
                if mask & (1usize << cpu_id) != 0 && !cpu_ids.contains(&cpu_id) {
                    cpu_ids.push(cpu_id);
                }
            }
        } else if !cpu_ids.contains(phys_id) {
            cpu_ids.push(*phys_id);
        }
    }
    cpu_ids
}

pub(crate) fn default_vcpu_affinities(
    cpu_num: usize,
    phys_cpu_ids: Option<&[usize]>,
    phys_cpu_sets: Option<&[usize]>,
) -> Vec<(usize, Option<usize>, usize)> {
    let mut vcpus = Vec::with_capacity(cpu_num);
    for vcpu_id in 0..cpu_num {
        vcpus.push((vcpu_id, None, vcpu_id));
    }

    if let Some(phys_cpu_sets) = phys_cpu_sets {
        for (vcpu_id, pcpu_mask_bitmap) in phys_cpu_sets.iter().enumerate() {
            if let Some(vcpu) = vcpus.get_mut(vcpu_id) {
                vcpu.1 = Some(*pcpu_mask_bitmap);
            }
        }
    }

    if let Some(phys_cpu_ids) = phys_cpu_ids {
        for (vcpu_id, phys_id) in phys_cpu_ids.iter().enumerate() {
            if let Some(vcpu) = vcpus.get_mut(vcpu_id) {
                vcpu.2 = *phys_id;
            }
        }
    }

    vcpus
}
