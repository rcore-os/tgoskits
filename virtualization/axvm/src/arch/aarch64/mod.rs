//! AxVM AArch64 adapter.
//!
//! This module owns the AxVM/ArceOS glue for the OS-neutral `arm_vcpu` core:
//! `AxvmArmHostOps` supplies host IRQ/GIC operations, while this module handles
//! `arm_vcpu` exits inside the AArch64 architecture boundary.

use alloc::sync::Arc;

use arm_vcpu::{
    ArmAccessWidth, ArmDataAbort, ArmDataAccessResult, ArmGicCpuInterfaceRegister,
    ArmGuestPhysAddr, ArmHostOps, ArmNestedPagingConfig, ArmPerCpu, ArmSysRegAddr, ArmVcpu,
    ArmVcpuCreateConfig, ArmVcpuError, ArmVcpuResult, ArmVcpuSetupConfig, ArmVmExit,
};
use ax_kernel_guard::IrqSave;
use ax_memory_addr::VirtAddr;
use axvm_types::{
    AccessWidth, GuestPhysAddr, NestedPagingConfig, SysRegAddr, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError as BackendError, VmBackendResult as BackendResult,
};

use super::{ArchOps, BoundVcpuExit, HypercallExit, VcpuRunAction, VcpuScheduling};
use crate::{AxVmResult, ax_err};

mod capabilities;
#[path = "../../architecture/cpu_up.rs"]
mod cpu_up;
mod data_abort;
pub(crate) mod fdt;
mod gic;
mod images;
mod ipi;
#[path = "../../architecture/nested_page_fault.rs"]
mod nested_page_fault;
mod npt;
#[path = "../../machine/ns16550_model.rs"]
mod ns16550_model;
mod pl011;
mod placement;
#[path = "../../architecture/sysreg.rs"]
mod sysreg;
mod timer;
mod vm;
#[path = "../../architecture/timer_scheduler.rs"]
mod vm_timer_scheduler;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
use cpu_up::{CpuUpExit, CpuUpOps};
pub use fdt::current_host_platform_snapshot;
pub use images::ImageLoader;
use ipi::SendIpiExit;
pub use pl011::{Aarch64Pl011Model, pl011_device_requirements};
use sysreg::{SysRegReadExit, SysRegWriteExit};

/// Stable model identifier for an AArch64 Synopsys DesignWare APB UART.
pub const DW_APB_UART_MODEL_ID: &str = ns16550_model::DW_APB_UART_MODEL_ID;

/// Returns named resources for a DesignWare APB virtual console.
pub fn dw_apb_uart_device_requirements()
-> axdevice::DeviceManagerResult<axdevice::DeviceRequirements> {
    ns16550_model::ns16550_device_requirements(0x100)
}

/// Returns named resources for a packed AArch64 NS16550 console.
pub fn ns16550_device_requirements() -> axdevice::DeviceManagerResult<axdevice::DeviceRequirements>
{
    ns16550_model::ns16550_device_requirements(0x100)
}

/// Returns the deterministic resource pools for an AArch64 virtual machine.
pub fn standard_machine_profile()
-> crate::machine::MachinePlanResult<crate::machine::MachineProfile> {
    let controller = crate::machine::Aarch64GicV3Profile::new(
        crate::machine::AddressRange::new(0x0800_0000, 0x0001_0000)?,
        0x080a_0000,
        0x0002_0000,
        Some(crate::machine::AddressRange::new(0x0808_0000, 0x0002_0000)?),
        480,
    )?;
    Ok(crate::machine::MachineProfile::new(
        crate::machine::AddressRange::new(0x0900_0000, 0x0100_0000)?,
        32..=511,
    )?
    .with_interrupt_controller(crate::machine::InterruptControllerProfile::Aarch64GicV3(
        controller,
    )))
}

pub(crate) struct Aarch64Arch;

#[derive(Debug, Default)]
pub(crate) struct VmArchConfig {
    interrupt_roles: Option<gic::Aarch64InterruptRoles>,
}

impl VmArchConfig {
    pub(crate) const fn new() -> Self {
        Self {
            interrupt_roles: None,
        }
    }

    pub(crate) fn reset_prepared_boot_state(&mut self) {
        self.interrupt_roles = None;
    }

    pub(crate) fn validate_prepared_boot_state(
        &self,
        _physical_interrupt_policy: axvm_types::PhysicalInterruptPolicy,
    ) -> AxVmResult {
        if self.interrupt_roles.is_none() {
            return ax_err!(InvalidInput, "AArch64 interrupt roles were not prepared");
        }
        Ok(())
    }

    pub(crate) fn set_interrupt_roles(&mut self, roles: gic::Aarch64InterruptRoles) {
        self.interrupt_roles = Some(roles);
    }

    pub(crate) const fn interrupt_roles(&self) -> Option<&gic::Aarch64InterruptRoles> {
        self.interrupt_roles.as_ref()
    }
}

pub(crate) struct VmArchState {
    gic_controller: Option<Arc<arm_vgic::GicV3Controller>>,
    host_spi_forwarding: Option<gic::HostSpiForwarding>,
    maintenance_interrupt: Option<gic::HostMaintenanceInterrupt>,
}

impl VmArchState {
    pub(crate) const fn new() -> Self {
        Self {
            gic_controller: None,
            host_spi_forwarding: None,
            maintenance_interrupt: None,
        }
    }

    pub(crate) fn set_gic_controller(
        &mut self,
        controller: Arc<arm_vgic::GicV3Controller>,
        host_spi_forwarding: Option<gic::HostSpiForwarding>,
        maintenance_interrupt: gic::HostMaintenanceInterrupt,
    ) {
        self.gic_controller = Some(controller);
        self.host_spi_forwarding = host_spi_forwarding;
        self.maintenance_interrupt = Some(maintenance_interrupt);
    }

    pub(crate) fn gic_controller(&self) -> Option<Arc<arm_vgic::GicV3Controller>> {
        self.gic_controller.clone()
    }
}

pub(crate) struct VmRuntimeArchState;

impl VmRuntimeArchState {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) const fn register_vcpu(&self, _vcpu_id: usize) {}
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Aarch64DeferredRunWork {
    ExternalInterrupt,
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

    fn synchronize_interrupts_after_exit(
        topology: &axdevice::InterruptTopology,
        vcpu: axdevice::VcpuInterruptId,
        exit: &ArmVmExit,
    ) -> AxVmResult {
        match exit {
            // The binding must harvest the live LR and apply DIR as one
            // operation. A generic synchronization here would save and reload
            // the same CPU interface twice for every trapped deactivation.
            ArmVmExit::DeactivateInterrupt { .. } => Ok(()),
            _ => {
                topology.synchronize_vcpu(vcpu)?;
                Ok(())
            }
        }
    }

    fn with_vcpu_interrupt_context<T>(vm: &crate::AxVMRef, run: impl FnOnce() -> T) -> T {
        let _ = vm;
        // ICH registers are banked per physical CPU, not per Rust task. Keep
        // the complete load/synchronize/unload transaction atomic with
        // respect to host IRQ dispatch so an interrupt cannot observe or
        // mutate a partially switched vCPU interface. Guest IRQs still exit
        // through HCR_EL2 routing while the guest is running.
        let _irq_guard = IrqSave::new();
        run()
    }

    fn after_external_interrupt(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        _vector: usize,
    ) {
        gic::handle_current_irq();
        crate::check_timer_events();
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
            ArmVmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
            ArmVmExit::DataAbort { abort } => data_abort::handle(vm, vcpu, abort),
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
            ArmVmExit::GicCpuInterfaceRead {
                register,
                destination,
            } => {
                let value = read_gic_cpu_interface_register(vcpu.id(), register)?;
                vcpu.set_gpr(destination, value as usize);
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::GicCpuInterfaceWrite { register, value } => {
                write_gic_cpu_interface_register(vcpu.id(), register, value)?;
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::DeactivateInterrupt { intid } => {
                vm.prepared_interrupt_topology()?
                    .deactivate_vcpu_interrupt(
                        axdevice::VcpuInterruptId::new(vcpu.id()),
                        axdevice::GuestInterruptId::new(intid),
                    )?;
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::ExternalInterrupt => {
                debug!("VM[{}] run VCpu[{}] handles a host IRQ", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Defer(
                    Aarch64DeferredRunWork::ExternalInterrupt,
                ))
            }
            ArmVmExit::CpuDown { state } => {
                warn!(
                    "VM[{}] run VCpu[{}] CpuDown state {state:#x}",
                    vm.id(),
                    vcpu.id()
                );
                Ok(BoundVcpuExit::Complete(VcpuRunAction::wait_for_event()))
            }
            ArmVmExit::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => cpu_up::handle::<Self>(
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
                Ok(BoundVcpuExit::Complete(VcpuRunAction::new(
                    VcpuScheduling::Resume,
                    Some(crate::StopReason::SystemDown),
                )))
            }
            ArmVmExit::SendIPI { value } => {
                ipi::handle(vm, vcpu.id(), SendIpiExit { sgi1r: value })
            }
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
            Aarch64DeferredRunWork::ExternalInterrupt => {
                Self::after_external_interrupt(vm, vcpu, 0);
            }
        }
        Ok(VcpuRunAction::resume())
    }
}

fn read_gic_cpu_interface_register(
    vcpu: usize,
    register: ArmGicCpuInterfaceRegister,
) -> AxVmResult<u64> {
    gic::read_cpu_interface_register(vcpu, register)
        .map_err(|error| crate::AxVmError::interrupt("read GICv3 CPU interface", error))
}

fn write_gic_cpu_interface_register(
    vcpu: usize,
    register: ArmGicCpuInterfaceRegister,
    value: u64,
) -> AxVmResult {
    gic::write_cpu_interface_register(vcpu, register, value)
        .map_err(|error| crate::AxVmError::interrupt("write GICv3 CPU interface", error))
}

struct AxvmArmHostOps;

impl ArmHostOps for AxvmArmHostOps {
    fn handle_current_host_irq() {
        gic::handle_current_irq();
    }
}

pub(crate) struct AxvmArmVcpu(ArmVcpu<AxvmArmHostOps>);

impl AxvmArmVcpu {
    fn complete_data_abort(
        &mut self,
        abort: ArmDataAbort,
        result: ArmDataAccessResult,
    ) -> BackendResult {
        arm_result(self.0.complete_data_abort(abort, result))
    }

    fn inject_external_data_abort(&mut self, abort: ArmDataAbort) -> BackendResult {
        arm_result(self.0.inject_external_data_abort(abort))
    }
}

impl VmArchVcpuOps for AxvmArmVcpu {
    type CreateConfig = ArmVcpuCreateConfig;
    type SetupConfig = ArmVcpuSetupConfig;
    type Exit = ArmVmExit;

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

    fn run(&mut self) -> BackendResult<Self::Exit> {
        arm_result(self.0.run())
    }

    fn bind(&mut self) -> BackendResult {
        arm_result(self.0.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        arm_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
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

    fn hardware_enable(&mut self) -> BackendResult {
        arm_result(self.0.hardware_enable::<AxvmArmHostOps>())
    }

    fn hardware_disable(&mut self) -> BackendResult {
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
