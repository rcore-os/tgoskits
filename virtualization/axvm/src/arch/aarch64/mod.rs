//! AxVM AArch64 adapter.
//!
//! This module owns the AxVM/ArceOS glue for the OS-neutral `arm_vcpu` core:
//! `AxvmArmHostOps` supplies host IRQ/GIC operations, while this module handles
//! `arm_vcpu` exits inside the AArch64 architecture boundary.

use alloc::boxed::Box;
use core::time::Duration;

use arm_vcpu::{
    ArmAccessWidth, ArmGuestPhysAddr, ArmHostOps, ArmNestedPagingConfig, ArmPerCpu, ArmSysRegAddr,
    ArmVcpu, ArmVcpuCreateConfig, ArmVcpuError, ArmVcpuResult, ArmVcpuSetupConfig, ArmVmExit,
};
use arm_vgic::host::ArmVgicHostIf;
use ax_crate_interface::impl_interface;
use ax_errno::{AxError, AxResult, ax_err};
use ax_memory_addr::{PhysAddr, VirtAddr};
use axvm_types::{
    AccessWidth, GuestPhysAddr, NestedPagingConfig, SysRegAddr, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps,
};

use super::{
    ArchOps, CpuUpExit, HypercallExit, MmioReadExit, MmioWriteExit, SendIpiExit, SysRegReadExit,
    SysRegWriteExit, VcpuCreateContext, VcpuRunAction, VcpuSetupContext, target_phys_cpu_ids,
};
use crate::host::{HostCpu, HostMemory, HostTime, default_host, gic};

mod npt;

pub(crate) struct Aarch64Arch;

impl ArchOps for Aarch64Arch {
    type VCpu = AxvmArmVcpu;
    type PerCpu = AxvmArmPerCpu;
    type VcpuCreateState = ();
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        arm_vcpu::has_hardware_support()
    }

    fn max_guest_page_table_levels() -> usize {
        arm_vcpu::max_guest_page_table_levels()
    }

    fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxResult<usize> {
        let mut selected = usize::MAX;
        for cpu_id in target_phys_cpu_ids(vcpu_mappings) {
            let levels = crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
                .unwrap_or_else(arm_vcpu::max_guest_page_table_levels);
            if levels == 0 {
                return ax_err!(
                    Unsupported,
                    "AArch64 nested paging is not enabled on target CPU"
                );
            }
            selected = selected.min(levels);
        }
        if selected == usize::MAX {
            selected = arm_vcpu::max_guest_page_table_levels();
        }
        match selected {
            3 | 4 => Ok(selected),
            _ => ax_err!(Unsupported, "unsupported AArch64 stage-2 page-table levels"),
        }
    }

    fn nested_paging_config(
        root_paddr: PhysAddr,
        levels: usize,
        vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<NestedPagingConfig> {
        let mut pa_bits = usize::MAX;
        for cpu_id in target_phys_cpu_ids(vcpu_mappings) {
            let bits =
                crate::percpu::cpu_guest_phys_addr_bits(cpu_id).unwrap_or_else(arm_vcpu::pa_bits);
            pa_bits = pa_bits.min(bits);
        }
        if pa_bits == usize::MAX {
            pa_bits = arm_vcpu::pa_bits();
        }

        let gpa_bits = match levels {
            3 => 39,
            4 => 48,
            _ => return ax_err!(InvalidInput, "unsupported AArch64 stage-2 levels"),
        };
        Ok(NestedPagingConfig::new(
            root_paddr, levels, gpa_bits, pa_bits,
        ))
    }

    fn new_nested_page_table(levels: usize) -> AxResult<Self::NestedPageTable> {
        npt::NestedPageTable::new(levels)
    }

    fn clean_dcache_range(addr: VirtAddr, size: usize) {
        aarch64_cpu_ext::cache::dcache_range(
            aarch64_cpu_ext::cache::CacheOp::Clean,
            addr.as_usize(),
            size,
        );
    }

    fn new_vcpu_create_state(
        _vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<Self::VcpuCreateState> {
        Ok(())
    }

    fn build_vcpu_create_config(
        _state: &Self::VcpuCreateState,
        ctx: VcpuCreateContext,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::CreateConfig> {
        Ok(ArmVcpuCreateConfig {
            mpidr_el1: ctx.phys_cpu_id as _,
            dtb_addr: ctx.dtb_addr.unwrap_or_default().as_usize(),
        })
    }

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::SetupConfig> {
        let passthrough = ctx.interrupt_mode == axvm_types::VMInterruptMode::Passthrough;
        Ok(ArmVcpuSetupConfig {
            passthrough_interrupt: passthrough,
            passthrough_timer: passthrough,
        })
    }

    fn ipi_targets(
        vm: &crate::AxVMRef,
        current_vcpu_id: usize,
        target_cpu: u64,
        target_cpu_aux: u64,
        send_to_all: bool,
        send_to_self: bool,
    ) -> crate::CpuMask<64> {
        let mut targets = crate::CpuMask::new();
        if send_to_all {
            for vcpu in vm.vcpu_list() {
                if vcpu.id() != current_vcpu_id {
                    targets.set(vcpu.id(), true);
                }
            }
        } else if send_to_self {
            targets.set(current_vcpu_id, true);
        } else {
            for (vcpu_id, _, phys_id) in vm.get_vcpu_affinities_pcpu_ids() {
                let affinity = phys_id as u64;
                let aff0 = affinity & 0xff;
                let aff123 = affinity & !0xff;
                if aff123 == target_cpu && aff0 < 16 && (target_cpu_aux & (1u64 << aff0)) != 0 {
                    targets.set(vcpu_id, true);
                }
            }
        }
        targets
    }

    fn handle_vcpu_exit(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxResult<VcpuRunAction> {
        match exit {
            ArmVmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
            ArmVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => super::handle_mmio_read(
                vm,
                vcpu,
                MmioReadExit {
                    addr: arm_guest_phys_addr_to_ax(addr),
                    width: arm_access_width_to_ax(width),
                    reg,
                    reg_width: arm_access_width_to_ax(reg_width),
                    signed_ext,
                },
            ),
            ArmVmExit::MmioWrite { addr, width, data } => super::handle_mmio_write(
                vm,
                MmioWriteExit {
                    addr: arm_guest_phys_addr_to_ax(addr),
                    width: arm_access_width_to_ax(width),
                    data,
                },
            ),
            ArmVmExit::SysRegRead { addr, reg } => super::handle_sys_reg_read(
                vm,
                vcpu,
                SysRegReadExit {
                    addr: arm_sys_reg_addr_to_ax(addr),
                    reg,
                },
            ),
            ArmVmExit::SysRegWrite { addr, value } => super::handle_sys_reg_write(
                vm,
                SysRegWriteExit {
                    addr: arm_sys_reg_addr_to_ax(addr),
                    value,
                },
            ),
            ArmVmExit::ExternalInterrupt { vector } => {
                debug!("VM[{}] run VCpu[{}] get irq {vector}", vm.id(), vcpu.id());
                Self::after_external_interrupt(vm, vcpu, vector as usize);
                Ok(VcpuRunAction::Yield)
            }
            ArmVmExit::CpuDown { state } => {
                warn!(
                    "VM[{}] run VCpu[{}] CpuDown state {state:#x}",
                    vm.id(),
                    vcpu.id()
                );
                Ok(VcpuRunAction::Wait)
            }
            ArmVmExit::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => super::handle_cpu_up::<Self>(
                vm,
                vcpu,
                CpuUpExit {
                    target_cpu,
                    entry_point: arm_guest_phys_addr_to_ax(entry_point),
                    arg,
                },
            ),
            ArmVmExit::SystemDown => {
                warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                Ok(VcpuRunAction::Stop(crate::StopReason::SystemDown))
            }
            ArmVmExit::SendIPI {
                target_cpu,
                target_cpu_aux,
                send_to_all,
                send_to_self,
                vector,
            } => super::handle_send_ipi::<Self>(
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
            ArmVmExit::Nothing => Ok(VcpuRunAction::Yield),
            _ => ax_err!(Unsupported, "unsupported AArch64 VM exit"),
        }
    }
}

struct AxvmArmHostOps;

impl ArmHostOps for AxvmArmHostOps {
    fn inject_virtual_interrupt(vector: u8) -> ArmVcpuResult {
        gic::inject_interrupt(vector as usize);
        Ok(())
    }

    fn fetch_pending_host_irq() -> Option<usize> {
        Some(gic::fetch_irq())
    }

    fn handle_current_host_irq() {
        gic::handle_current_irq();
    }
}

pub(crate) struct AxvmArmVcpu(ArmVcpu<AxvmArmHostOps>);

impl VmArchVcpuOps for AxvmArmVcpu {
    type CreateConfig = ArmVcpuCreateConfig;
    type SetupConfig = ArmVcpuSetupConfig;
    type Exit = ArmVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> AxResult<Self> {
        arm_result(ArmVcpu::new(vm_id, vcpu_id, config)).map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        arm_result(self.0.set_entry(ax_guest_phys_addr_to_arm(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> AxResult {
        arm_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_arm(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        arm_result(self.0.setup(config))
    }

    fn run(&mut self) -> AxResult<Self::Exit> {
        arm_result(self.0.run())
    }

    fn bind(&mut self) -> AxResult {
        arm_result(self.0.bind())
    }

    fn unbind(&mut self) -> AxResult {
        arm_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        arm_result(self.0.inject_interrupt(vector))
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

pub(crate) struct AxvmArmPerCpu(ArmPerCpu);

impl VmArchPerCpuOps for AxvmArmPerCpu {
    fn new(cpu_id: usize) -> AxResult<Self> {
        arm_result(ArmPerCpu::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> AxResult {
        arm_result(self.0.hardware_enable::<AxvmArmHostOps>())
    }

    fn hardware_disable(&mut self) -> AxResult {
        arm_result(self.0.hardware_disable())
    }

    fn max_guest_page_table_levels(&self) -> usize {
        self.0.max_guest_page_table_levels()
    }

    fn guest_phys_addr_bits(&self) -> usize {
        self.0.guest_phys_addr_bits()
    }
}

fn arm_result<T>(result: ArmVcpuResult<T>) -> AxResult<T> {
    result.map_err(arm_error_to_ax)
}

fn arm_error_to_ax(err: ArmVcpuError) -> AxError {
    match err {
        ArmVcpuError::InvalidInput => AxError::InvalidInput,
        ArmVcpuError::Unsupported => AxError::Unsupported,
        ArmVcpuError::BadState => AxError::BadState,
    }
}

fn ax_guest_phys_addr_to_arm(addr: GuestPhysAddr) -> ArmGuestPhysAddr {
    ArmGuestPhysAddr::from_usize(addr.as_usize())
}

fn arm_guest_phys_addr_to_ax(addr: ArmGuestPhysAddr) -> GuestPhysAddr {
    GuestPhysAddr::from(addr.as_usize())
}

fn ax_nested_paging_to_arm(config: NestedPagingConfig) -> ArmNestedPagingConfig {
    ArmNestedPagingConfig::new(
        config.root_paddr.as_usize(),
        config.levels,
        config.gpa_bits,
        config.mode,
    )
}

fn arm_access_width_to_ax(width: ArmAccessWidth) -> AccessWidth {
    match width {
        ArmAccessWidth::Byte => AccessWidth::Byte,
        ArmAccessWidth::Word => AccessWidth::Word,
        ArmAccessWidth::Dword => AccessWidth::Dword,
        ArmAccessWidth::Qword => AccessWidth::Qword,
    }
}

fn arm_sys_reg_addr_to_ax(addr: ArmSysRegAddr) -> SysRegAddr {
    SysRegAddr::new(addr.addr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_arm_vcpu_errors_to_ax_errors() {
        assert_eq!(
            arm_error_to_ax(ArmVcpuError::InvalidInput),
            AxError::InvalidInput
        );
        assert_eq!(
            arm_error_to_ax(ArmVcpuError::Unsupported),
            AxError::Unsupported
        );
        assert_eq!(arm_error_to_ax(ArmVcpuError::BadState), AxError::BadState);
    }

    fn assert_arm_exit_type<T: VmArchVcpuOps<Exit = ArmVmExit>>() {}

    #[test]
    fn axvm_arm_vcpu_uses_arm_exit_type() {
        assert_arm_exit_type::<AxvmArmVcpu>();
    }

    #[test]
    fn converts_arm_value_types_to_axvm_value_types() {
        assert_eq!(
            arm_guest_phys_addr_to_ax(ArmGuestPhysAddr::from_usize(0x4000)).as_usize(),
            0x4000
        );
        assert_eq!(
            arm_access_width_to_ax(ArmAccessWidth::Dword),
            AccessWidth::Dword
        );
        assert_eq!(
            arm_access_width_to_ax(ArmAccessWidth::Qword),
            AccessWidth::Qword
        );
        assert_eq!(
            arm_sys_reg_addr_to_ax(ArmSysRegAddr::new(0x3a_3016)).addr(),
            0x3a_3016
        );
    }
}

struct ArmVgicHostIfImpl;

#[impl_interface]
impl ArmVgicHostIf for ArmVgicHostIfImpl {
    fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
        default_host().alloc_contiguous_frames(frame_count, frame_align)
    }

    fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
        default_host().dealloc_contiguous_frames(start_paddr, frame_count);
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        default_host().phys_to_virt(paddr)
    }

    fn host_cpu_num() -> usize {
        default_host().cpu_count()
    }

    fn current_vcpu_id() -> usize {
        crate::current_vcpu_id().expect("current AArch64 vCPU is not set")
    }

    fn current_time_nanos() -> u64 {
        default_host().monotonic_time().as_nanos() as u64
    }

    fn register_timer(deadline: Duration, callback: Box<dyn FnOnce(Duration) + Send + 'static>) {
        let _ = default_host().register_timer(deadline.as_nanos() as u64, callback);
    }

    fn read_vgicd_iidr() -> u32 {
        gic::read_gicd_iidr()
    }

    fn read_vgicd_typer() -> u32 {
        gic::read_gicd_typer()
    }

    fn get_host_gicd_base() -> PhysAddr {
        gic::host_gicd_base()
    }

    fn get_host_gicr_base() -> PhysAddr {
        gic::host_gicr_base()
    }

    fn hardware_inject_virtual_interrupt(vector: u8) {
        gic::inject_interrupt(vector as usize);
    }
}
