use alloc::boxed::Box;
use core::time::Duration;

use arm_vcpu::host::ArmVcpuHostIf;
use arm_vgic::host::ArmVgicHostIf;
use ax_crate_interface::impl_interface;
use ax_errno::{AxResult, ax_err};
use ax_memory_addr::{PhysAddr, VirtAddr};
use axvm_types::NestedPagingConfig;

use super::{ArchOps, VcpuCreateContext, VcpuSetupContext, target_phys_cpu_ids};
use crate::host::{HostCpu, HostMemory, HostTime, default_host, gic};

mod npt;

pub(crate) struct Aarch64Arch;

impl ArchOps for Aarch64Arch {
    type VCpu = arm_vcpu::Aarch64VCpu;
    type PerCpu = arm_vcpu::Aarch64PerCpu;
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
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::CreateConfig> {
        Ok(arm_vcpu::Aarch64VCpuCreateConfig {
            mpidr_el1: ctx.phys_cpu_id as _,
            dtb_addr: ctx.dtb_addr.unwrap_or_default().as_usize(),
        })
    }

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::SetupConfig> {
        let passthrough = ctx.interrupt_mode == axvm_types::VMInterruptMode::Passthrough;
        Ok(arm_vcpu::Aarch64VCpuSetupConfig {
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
}

struct ArmVcpuHostIfImpl;

#[impl_interface]
impl ArmVcpuHostIf for ArmVcpuHostIfImpl {
    fn hardware_inject_virtual_interrupt(vector: u8) {
        gic::inject_interrupt(vector as usize);
    }

    fn fetch_irq() -> usize {
        gic::fetch_irq()
    }

    fn handle_irq() {
        gic::handle_current_irq();
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
