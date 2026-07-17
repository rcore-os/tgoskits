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
use ax_cpu_local::CpuPin;
use ax_crate_interface::impl_interface;
use ax_memory_addr::{PhysAddr, VirtAddr};
use axvm_types::{
    AccessWidth, GuestPhysAddr, NestedPagingConfig, SysRegAddr, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError as BackendError, VmBackendResult as BackendResult,
};

use super::{
    ArchOps, BoundVcpuExit, CommonDeferredRunWork, HypercallExit, MmioReadExit, MmioWriteExit,
    VcpuRunAction,
};
use crate::{
    AxVmResult, ax_err,
    host::{HostCpu, HostMemory, HostTime, default_host},
};

mod capabilities;
#[path = "../../architecture/cpu_up.rs"]
mod cpu_up;
pub(crate) mod fdt;
mod gic;
mod images;
mod ipi;
mod npt;
#[path = "../../architecture/sysreg.rs"]
mod sysreg;
mod vm;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
use cpu_up::{CpuUpExit, CpuUpOps};
pub use images::ImageLoader;
use ipi::SendIpiExit;
use sysreg::{SysRegReadExit, SysRegWriteExit};

pub(crate) struct Aarch64Arch;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Aarch64DeferredRunWork {
    Common(CommonDeferredRunWork),
    SysReg(sysreg::DeferredRunWork),
    CpuUp(CpuUpExit),
    SendIpi(SendIpiExit),
}

impl From<CommonDeferredRunWork> for Aarch64DeferredRunWork {
    fn from(work: CommonDeferredRunWork) -> Self {
        Self::Common(work)
    }
}

impl From<sysreg::DeferredRunWork> for Aarch64DeferredRunWork {
    fn from(work: sysreg::DeferredRunWork) -> Self {
        Self::SysReg(work)
    }
}

impl CpuUpOps for Aarch64Arch {}

impl ArchOps for Aarch64Arch {
    type VCpu = AxvmArmVcpu;
    type PerCpu = AxvmArmPerCpu;
    type DeferredRunWork = Aarch64DeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        arm_vcpu::has_hardware_support()
    }

    fn clean_dcache_range(addr: VirtAddr, size: usize) {
        aarch64_cpu_ext::cache::dcache_range(
            aarch64_cpu_ext::cache::CacheOp::Clean,
            addr.as_usize(),
            size,
        );
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn activate_guest_irq_routes(vm: &crate::AxVMRef) -> AxVmResult {
        vm::activate_guest_irq_routes(vm)
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn revoke_guest_irq_routes(vm: &crate::AxVMRef) -> AxVmResult {
        vm::revoke_guest_irq_routes(vm)
    }

    fn handle_vcpu_exit_bound<'cpu>(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit<'cpu>,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>
    where
        Self::VCpu: 'cpu,
    {
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
            } => super::handle_mmio_read::<Self>(
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
            ArmVmExit::MmioWrite { addr, width, data } => super::handle_mmio_write::<Self>(
                vm,
                vcpu,
                MmioWriteExit {
                    addr: arm_guest_phys_addr_to_ax(addr),
                    width: arm_access_width_to_ax(width),
                    data,
                },
            ),
            ArmVmExit::SysRegRead { addr, reg } => sysreg::handle_read(
                vm,
                vcpu,
                SysRegReadExit {
                    addr: arm_sys_reg_addr_to_ax(addr),
                    reg,
                },
            ),
            ArmVmExit::SysRegWrite { addr, value } => sysreg::handle_write(
                vm,
                SysRegWriteExit {
                    addr: arm_sys_reg_addr_to_ax(addr),
                    value,
                },
            ),
            ArmVmExit::ExternalInterrupt => Ok(BoundVcpuExit::Complete(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
            })),
            ArmVmExit::CpuDown { state } => {
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
            ArmVmExit::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => Ok(BoundVcpuExit::Defer(Aarch64DeferredRunWork::CpuUp(
                CpuUpExit {
                    target_cpu,
                    entry_point: arm_guest_phys_addr_to_ax(entry_point),
                    arg,
                },
            ))),
            ArmVmExit::SystemDown => {
                warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: Some(crate::StopReason::SystemDown),
                }))
            }
            ArmVmExit::SendIPI {
                target_cpu,
                target_cpu_aux,
                send_to_all,
                send_to_self,
                vector,
            } => Ok(BoundVcpuExit::Defer(Aarch64DeferredRunWork::SendIpi(
                SendIpiExit {
                    target_cpu,
                    target_cpu_aux,
                    send_to_all,
                    send_to_self,
                    vector,
                },
            ))),
            ArmVmExit::Nothing => Ok(BoundVcpuExit::Continue),
            _ => ax_err!(Unsupported, "unsupported AArch64 VM exit"),
        }
    }

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction> {
        match work {
            Aarch64DeferredRunWork::Common(work) => super::finish_deferred::<Self>(vm, vcpu, work),
            Aarch64DeferredRunWork::SysReg(work) => sysreg::finish(vm, vcpu, work),
            Aarch64DeferredRunWork::CpuUp(exit) => cpu_up::finish::<Self>(vm, vcpu, exit),
            Aarch64DeferredRunWork::SendIpi(exit) => ipi::finish(vm, vcpu.id(), exit),
        }
    }
}

struct AxvmArmHostOps;

impl ArmHostOps for AxvmArmHostOps {
    fn inject_virtual_interrupt(vector: u8) -> ArmVcpuResult {
        gic::inject_interrupt(vector as usize);
        Ok(())
    }

    unsafe fn handle_post_unbind_host_irq(cpu_pin: &CpuPin) -> ArmVcpuResult {
        // SAFETY: ArmVcpu invokes this callback only after AxVM restored host
        // anchors, unbound the backend, cleared CURRENT_VCPU, and retained the
        // same CPU pin plus the lower-EL saved DAIF owner.
        let permit = unsafe {
            ax_std::os::arceos::modules::ax_hal::irq::PinnedHostIrqPermit::from_post_unbind(
                0, cpu_pin,
            )
        };
        ax_std::os::arceos::modules::ax_hal::irq::handle_pinned_host_irq(permit);
        Ok(())
    }

    unsafe fn handle_current_host_irq() {
        // SAFETY: ArmHostOps invokes this unsafe callback only from the
        // current-EL exception vector, whose frame owns the interrupted DAIF.
        let permit = unsafe {
            ax_std::os::arceos::modules::ax_hal::cpu::trap::TrapIrqPermit::from_arch_entry(0)
        };
        ax_std::os::arceos::modules::ax_hal::irq::handle_trap_irq(permit);
    }
}

pub(crate) struct AxvmArmVcpu(ArmVcpu<AxvmArmHostOps>);

impl VmArchVcpuOps for AxvmArmVcpu {
    type CreateConfig = ArmVcpuCreateConfig;
    type SetupConfig = ArmVcpuSetupConfig;
    type Exit<'cpu> = ArmVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        arm_result(ArmVcpu::new(vm_id, vcpu_id, config)).map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        arm_result(self.0.set_entry(ax_guest_phys_addr_to_arm(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        arm_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_arm(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        arm_result(self.0.setup(config))
    }

    fn run<'cpu>(&'cpu mut self, cpu_pin: &'cpu CpuPin) -> BackendResult<Self::Exit<'cpu>> {
        arm_result(self.0.run(cpu_pin))
    }

    fn bind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        arm_result(self.0.bind(cpu_pin))
    }

    fn unbind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        arm_result(self.0.unbind(cpu_pin))
    }

    fn finish_post_unbind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        arm_result(self.0.finish_post_unbind(cpu_pin))
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        arm_result(self.0.inject_interrupt(vector))
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

pub(crate) struct AxvmArmPerCpu(ArmPerCpu);

impl VmArchPerCpuOps for AxvmArmPerCpu {
    fn new(cpu_id: usize) -> BackendResult<Self> {
        arm_result(ArmPerCpu::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self, _cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        arm_result(self.0.hardware_enable::<AxvmArmHostOps>())
    }

    fn hardware_disable(&mut self, _cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        arm_result(self.0.hardware_disable())
    }

    fn max_guest_page_table_levels(&self) -> usize {
        self.0.max_guest_page_table_levels()
    }

    fn guest_phys_addr_bits(&self) -> usize {
        self.0.guest_phys_addr_bits()
    }
}

fn arm_result<T>(result: ArmVcpuResult<T>) -> BackendResult<T> {
    result.map_err(arm_error_to_backend)
}

fn arm_error_to_backend(err: ArmVcpuError) -> BackendError {
    match err {
        ArmVcpuError::InvalidInput => BackendError::InvalidInput,
        ArmVcpuError::Unsupported => BackendError::Unsupported,
        ArmVcpuError::BadState => BackendError::InvalidState,
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
    fn converts_arm_vcpu_errors_to_backend_errors() {
        assert_eq!(
            arm_error_to_backend(ArmVcpuError::InvalidInput),
            BackendError::InvalidInput
        );
        assert_eq!(
            arm_error_to_backend(ArmVcpuError::Unsupported),
            BackendError::Unsupported
        );
        assert_eq!(
            arm_error_to_backend(ArmVcpuError::BadState),
            BackendError::InvalidState
        );
    }

    fn assert_arm_exit_type<T>()
    where
        for<'cpu> T: VmArchVcpuOps<Exit<'cpu> = ArmVmExit>,
    {
    }

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

    fn current_vm_id() -> usize {
        super::current_vcpu_identity_for_task()
            .unwrap_or_else(|error| panic!("current AArch64 vCPU identity is invalid: {error}"))
            .expect("current AArch64 VM is not set")
            .into_ids()
            .0
    }

    fn current_vcpu_id() -> usize {
        super::current_vcpu_identity_for_task()
            .unwrap_or_else(|error| panic!("current AArch64 vCPU identity is invalid: {error}"))
            .expect("current AArch64 vCPU is not set")
            .into_ids()
            .1
    }

    fn current_time_nanos() -> u64 {
        default_host().monotonic_time().as_nanos() as u64
    }

    fn register_timer(
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> Option<usize> {
        crate::timer::register_timer(deadline.as_nanos() as u64, callback)
    }

    fn cancel_timer(token: usize) {
        let _ = crate::timer::cancel_timer(token);
    }

    fn queue_virtual_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) {
        if let Err(err) = crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector) {
            warn!(
                "failed to queue AArch64 virtual interrupt {vector:#x} for VM[{vm_id}] \
                 VCpu[{vcpu_id}]: {err:?}"
            );
        }
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

    fn route_physical_spi(
        irq: u32,
        cpu_phys_id: usize,
        aff3: u8,
        aff2: u8,
        aff1: u8,
        aff0: u8,
    ) -> arm_vgic::VgicResult {
        gic::route_physical_spi(irq, cpu_phys_id, (aff3, aff2, aff1, aff0))
    }

    fn begin_physical_spi_quiesce(irq: u32) -> arm_vgic::VgicResult {
        gic::begin_physical_spi_quiesce(irq)
    }

    fn poll_physical_distributor_write_complete() -> arm_vgic::VgicResult<bool> {
        gic::poll_physical_distributor_write_complete()
    }
}
