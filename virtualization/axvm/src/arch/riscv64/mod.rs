use alloc::vec::Vec;

use ax_crate_interface::impl_interface;
use ax_errno::{AxError, AxResult, ax_err};
use ax_memory_addr::{PhysAddr, VirtAddr};
use axvm_types::{
    AccessWidth, GuestPhysAddr, MappingFlags, NestedPagingConfig, VCpuId, VMId, VMInterruptMode,
    VmArchPerCpuOps, VmArchVcpuOps,
};
use riscv_vcpu::{
    GprIndex as RiscvGprIndex, RiscvAccessFlags, RiscvAccessWidth, RiscvGuestPhysAddr,
    RiscvHostOps, RiscvHostPhysAddr, RiscvHostVirtAddr, RiscvNestedPagingConfig, RiscvPerCpu,
    RiscvVCpu, RiscvVcpuCreateConfig, RiscvVcpuError, RiscvVcpuResult, RiscvVmExit,
};
use riscv_vplic::host::RiscvVplicHostIf;

use super::{
    ArchOps, BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuCreateContext,
    VcpuRunAction, VcpuSetupContext,
};
use crate::{
    StopReason,
    architecture::ops::{default_vcpu_affinities, target_phys_cpu_ids},
    host::{HostMemory, default_host},
};

mod capabilities;
#[path = "../../architecture/cpu_up.rs"]
mod cpu_up;
mod images;
mod irq;
mod npt;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
use cpu_up::{CpuUpExit, CpuUpOps};
pub use images::ImageLoader;

pub(crate) struct Riscv64Arch;

#[derive(Clone, Copy, Debug)]
pub(crate) enum RiscvDeferredRunWork {
    ExternalInterrupt { vector: usize },
}

impl CpuUpOps for Riscv64Arch {
    fn set_cpu_up_success(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, 0);
    }
}

impl ArchOps for Riscv64Arch {
    type VCpu = AxvmRiscvVcpu;
    type PerCpu = AxvmRiscvPerCpu;
    type VcpuCreateState = ();
    type DeferredRunWork = RiscvDeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        riscv_vcpu::has_hardware_support()
    }

    fn guest_page_table_levels(vcpu_mappings: &[(usize, Option<usize>, usize)]) -> AxResult<usize> {
        let mut levels = riscv_vcpu::max_guest_page_table_levels();
        for cpu_id in target_phys_cpu_ids(vcpu_mappings) {
            levels = levels.min(
                crate::percpu::cpu_max_guest_page_table_levels(cpu_id)
                    .unwrap_or_else(riscv_vcpu::max_guest_page_table_levels),
            );
        }
        match levels {
            3 | 4 => Ok(levels),
            _ => ax_err!(Unsupported, "no supported RISC-V G-stage paging mode"),
        }
    }

    fn nested_paging_config(
        root_paddr: PhysAddr,
        levels: usize,
        _vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<NestedPagingConfig> {
        match levels {
            3 => Ok(NestedPagingConfig::new(root_paddr, 3, 41, 8)),
            4 => Ok(NestedPagingConfig::new(root_paddr, 4, 50, 9)),
            _ => ax_err!(InvalidInput, "unsupported RISC-V G-stage levels"),
        }
    }

    fn new_nested_page_table(levels: usize) -> AxResult<Self::NestedPageTable> {
        npt::NestedPageTable::new(levels)
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
        let (vcpu_id, _phys_cpu_id, dtb_addr, _firmware_boot) = ctx.into_parts();
        Ok(RiscvVcpuCreateConfig {
            hart_id: vcpu_id,
            dtb_addr: dtb_addr.unwrap_or_default().as_usize(),
        })
    }

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::SetupConfig> {
        let _ = ctx.into_parts();
        Ok(())
    }

    fn register_platform_irq_injector() {
        register_platform_irq_injector();
    }

    fn before_first_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        if vm.interrupt_mode() != VMInterruptMode::Passthrough {
            return;
        }
        let Some(cpu_id) = vcpu.phys_cpu_set().and_then(first_cpu_in_mask) else {
            warn!(
                "skip RISC-V virtual IRQ affinity for VM[{}] VCpu[{}]: no fixed host CPU",
                vm.id(),
                vcpu.id()
            );
            return;
        };
        let irq_sources = vm.with_config(|config| config.pass_through_irqs().to_vec());
        crate::irq::set_riscv_virtual_irq_targets(cpu_id, &irq_sources);
    }

    fn vcpu_affinities(
        cpu_num: usize,
        phys_cpu_ids: Option<&[usize]>,
        phys_cpu_sets: Option<&[usize]>,
    ) -> Vec<(usize, Option<usize>, usize)> {
        let mut vcpus = default_vcpu_affinities(cpu_num, phys_cpu_ids, phys_cpu_sets);
        if phys_cpu_sets.is_none() {
            for (_, mask, phys_id) in &mut vcpus {
                *mask = Some(1 << *phys_id);
            }
        }
        vcpus
    }

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>, vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, vcpu_id);
        vcpu.set_gpr(RiscvGprIndex::A1 as usize, arg);
    }

    fn after_external_interrupt(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        vector: usize,
    ) {
        vcpu.with_current_cpu_set(|| {
            crate::host::arceos::dispatch_host_irq(vector);
            vcpu.get_arch_vcpu().latch_hvip_from_hw();
        });
        crate::check_timer_events();
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
            RiscvVmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
            RiscvVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => super::handle_mmio_read(
                vm,
                vcpu,
                MmioReadExit {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    width: riscv_access_width_to_ax(width),
                    reg,
                    reg_width: riscv_access_width_to_ax(reg_width),
                    signed_ext,
                },
            ),
            RiscvVmExit::MmioWrite { addr, width, data } => super::handle_mmio_write::<Self>(
                vm,
                MmioWriteExit {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    width: riscv_access_width_to_ax(width),
                    data,
                },
            ),
            RiscvVmExit::NestedPageFault { addr, access_flags } => {
                handle_riscv_nested_page_fault(vm, vcpu, addr, access_flags)
            }
            RiscvVmExit::ExternalInterrupt { vector } => {
                debug!("VM[{}] run VCpu[{}] get irq {vector}", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Defer(
                    RiscvDeferredRunWork::ExternalInterrupt {
                        vector: vector as usize,
                    },
                ))
            }
            RiscvVmExit::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => cpu_up::handle::<Self>(
                vm,
                vcpu,
                CpuUpExit {
                    target_cpu,
                    entry_point: riscv_guest_phys_addr_to_ax(entry_point),
                    arg,
                },
            ),
            RiscvVmExit::CpuDown { state } => {
                warn!(
                    "VM[{}] run VCpu[{}] CpuDown state {state:#x}",
                    vm.id(),
                    vcpu.id()
                );
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: true,
                    stop_reason: None,
                }))
            }
            RiscvVmExit::Halt => {
                debug!("VM[{}] run VCpu[{}] Halt", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: true,
                    stop_reason: None,
                }))
            }
            RiscvVmExit::SystemDown => {
                warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: Some(StopReason::SystemDown),
                }))
            }
            RiscvVmExit::Nothing => Ok(BoundVcpuExit::Complete(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
            })),
        }
    }

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxResult<VcpuRunAction> {
        match work {
            RiscvDeferredRunWork::ExternalInterrupt { vector } => {
                Self::after_external_interrupt(vm, vcpu, vector);
            }
        }
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
    }
}

fn handle_riscv_nested_page_fault(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    addr: RiscvGuestPhysAddr,
    access_flags: RiscvAccessFlags,
) -> AxResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let ax_addr = riscv_guest_phys_addr_to_ax(addr);
    if vm.get_devices()?.find_mmio_dev(ax_addr).is_some() {
        let Some(decoded) = vcpu.get_arch_vcpu().decode_mmio_fault(addr, access_flags) else {
            warn!(
                "VM[{}] VCpu[{}] nested page fault at {:#x} maps MMIO but cannot be decoded",
                vm.id(),
                vcpu.id(),
                ax_addr.as_usize()
            );
            return Ok(BoundVcpuExit::Complete(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
            }));
        };
        return Riscv64Arch::handle_vcpu_exit_bound(vm, vcpu, decoded);
    }

    let ax_flags = riscv_access_flags_to_ax(access_flags);
    if vm.handle_nested_page_fault(ax_addr, ax_flags) {
        Ok(BoundVcpuExit::Continue)
    } else {
        warn!(
            "VM[{}] VCpu[{}] unhandled nested page fault at {:#x}, access={:?}",
            vm.id(),
            vcpu.id(),
            ax_addr.as_usize(),
            ax_flags
        );
        Ok(BoundVcpuExit::Complete(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        }))
    }
}

struct AxvmRiscvHostOps;

impl RiscvHostOps for AxvmRiscvHostOps {
    fn virt_to_phys(vaddr: RiscvHostVirtAddr) -> RiscvHostPhysAddr {
        RiscvHostPhysAddr::from_usize(
            default_host()
                .virt_to_phys(VirtAddr::from(vaddr.as_usize()))
                .as_usize(),
        )
    }
}

pub(crate) struct AxvmRiscvVcpu(RiscvVCpu<AxvmRiscvHostOps>);

impl AxvmRiscvVcpu {
    fn latch_hvip_from_hw(&mut self) {
        self.0.latch_hvip_from_hw();
    }

    fn decode_mmio_fault(
        &mut self,
        addr: RiscvGuestPhysAddr,
        access_flags: RiscvAccessFlags,
    ) -> Option<RiscvVmExit> {
        self.0.decode_mmio_fault(addr, access_flags)
    }
}

impl VmArchVcpuOps for AxvmRiscvVcpu {
    type CreateConfig = RiscvVcpuCreateConfig;
    type SetupConfig = ();
    type Exit = RiscvVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> AxResult<Self> {
        riscv_result(RiscvVCpu::new(vm_id, vcpu_id, config)).map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        riscv_result(self.0.set_entry(ax_guest_phys_addr_to_riscv(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> AxResult {
        riscv_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_riscv(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        riscv_result(self.0.setup(config))
    }

    fn run(&mut self) -> AxResult<Self::Exit> {
        riscv_result(self.0.run())
    }

    fn bind(&mut self) -> AxResult {
        riscv_result(self.0.bind())
    }

    fn unbind(&mut self) -> AxResult {
        riscv_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        riscv_result(self.0.inject_interrupt(vector))
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

pub(crate) struct AxvmRiscvPerCpu(RiscvPerCpu);

impl VmArchPerCpuOps for AxvmRiscvPerCpu {
    fn new(cpu_id: usize) -> AxResult<Self> {
        riscv_result(RiscvPerCpu::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> AxResult {
        riscv_result(self.0.hardware_enable())
    }

    fn hardware_disable(&mut self) -> AxResult {
        riscv_result(self.0.hardware_disable())
    }

    fn max_guest_page_table_levels(&self) -> usize {
        self.0.max_guest_page_table_levels()
    }

    fn guest_phys_addr_bits(&self) -> usize {
        self.0.guest_phys_addr_bits()
    }
}

fn riscv_result<T>(result: RiscvVcpuResult<T>) -> AxResult<T> {
    result.map_err(riscv_error_to_ax)
}

fn riscv_error_to_ax(err: RiscvVcpuError) -> AxError {
    match err {
        RiscvVcpuError::InvalidInput => AxError::InvalidInput,
        RiscvVcpuError::Unsupported => AxError::Unsupported,
        RiscvVcpuError::BadState => AxError::BadState,
        RiscvVcpuError::InvalidTrap
        | RiscvVcpuError::DecodeFailed
        | RiscvVcpuError::GuestMemoryFault => AxError::InvalidData,
    }
}

fn ax_guest_phys_addr_to_riscv(addr: GuestPhysAddr) -> RiscvGuestPhysAddr {
    RiscvGuestPhysAddr::from_usize(addr.as_usize())
}

fn riscv_guest_phys_addr_to_ax(addr: RiscvGuestPhysAddr) -> GuestPhysAddr {
    GuestPhysAddr::from(addr.as_usize())
}

fn ax_nested_paging_to_riscv(config: NestedPagingConfig) -> RiscvNestedPagingConfig {
    RiscvNestedPagingConfig::new(
        config.root_paddr.as_usize(),
        config.levels,
        config.gpa_bits,
        config.mode,
    )
}

fn riscv_access_width_to_ax(width: RiscvAccessWidth) -> AccessWidth {
    match width {
        RiscvAccessWidth::Byte => AccessWidth::Byte,
        RiscvAccessWidth::Word => AccessWidth::Word,
        RiscvAccessWidth::Dword => AccessWidth::Dword,
        RiscvAccessWidth::Qword => AccessWidth::Qword,
    }
}

fn riscv_access_flags_to_ax(flags: RiscvAccessFlags) -> MappingFlags {
    let mut converted = MappingFlags::empty();
    if flags.contains(RiscvAccessFlags::READ) {
        converted |= MappingFlags::READ;
    }
    if flags.contains(RiscvAccessFlags::WRITE) {
        converted |= MappingFlags::WRITE;
    }
    if flags.contains(RiscvAccessFlags::EXECUTE) {
        converted |= MappingFlags::EXECUTE;
    }
    if flags.contains(RiscvAccessFlags::USER) {
        converted |= MappingFlags::USER;
    }
    if flags.contains(RiscvAccessFlags::DEVICE) {
        converted |= MappingFlags::DEVICE;
    }
    if flags.contains(RiscvAccessFlags::UNCACHED) {
        converted |= MappingFlags::UNCACHED;
    }
    converted
}

struct RiscvVplicHostIfImpl;

#[impl_interface]
impl RiscvVplicHostIf for RiscvVplicHostIfImpl {
    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        default_host().phys_to_virt(paddr)
    }
}

fn register_platform_irq_injector() {
    crate::irq::register_riscv_virtual_irq_injector(inject_virtual_irq);
}

fn first_cpu_in_mask(mask: usize) -> Option<usize> {
    (mask != 0).then_some(mask.trailing_zeros() as usize)
}

fn inject_virtual_irq(irq_id: usize) -> bool {
    let Some(vm_id) = crate::current_vm_id() else {
        trace!("skip RISC-V virtual IRQ {irq_id}: no current VM context");
        return false;
    };

    debug!("injecting RISC-V virtual IRQ id: {irq_id}");

    let Some(injected) = crate::manager::with_vm(vm_id, |vm| {
        if let Err(err) = vm.pulse_interrupt(irq_id) {
            warn!("failed to inject RISC-V virtual IRQ {irq_id}: {err:?}");
            return false;
        }
        true
    }) else {
        warn!("cannot inject RISC-V virtual IRQ {irq_id}: VM[{vm_id}] not found");
        return false;
    };

    injected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_riscv_exit_type<T: VmArchVcpuOps<Exit = RiscvVmExit>>() {}

    #[test]
    fn axvm_riscv_vcpu_uses_riscv_exit_type() {
        assert_riscv_exit_type::<AxvmRiscvVcpu>();
    }

    #[test]
    fn converts_riscv_vcpu_errors_to_ax_errors() {
        assert_eq!(
            riscv_error_to_ax(RiscvVcpuError::InvalidInput),
            AxError::InvalidInput
        );
        assert_eq!(
            riscv_error_to_ax(RiscvVcpuError::Unsupported),
            AxError::Unsupported
        );
        assert_eq!(
            riscv_error_to_ax(RiscvVcpuError::BadState),
            AxError::BadState
        );
        assert_eq!(
            riscv_error_to_ax(RiscvVcpuError::DecodeFailed),
            AxError::InvalidData
        );
    }

    #[test]
    fn converts_riscv_value_types_to_axvm_value_types() {
        assert_eq!(
            riscv_guest_phys_addr_to_ax(RiscvGuestPhysAddr::from_usize(0x4000)).as_usize(),
            0x4000
        );
        assert_eq!(
            riscv_access_width_to_ax(RiscvAccessWidth::Dword),
            AccessWidth::Dword
        );
        assert_eq!(
            riscv_access_flags_to_ax(RiscvAccessFlags::READ | RiscvAccessFlags::WRITE),
            MappingFlags::READ | MappingFlags::WRITE
        );
    }
}
