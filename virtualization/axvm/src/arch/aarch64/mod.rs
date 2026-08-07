//! AxVM AArch64 adapter.
//!
//! This module owns the AxVM/ArceOS glue for the OS-neutral `arm_vcpu` core.
//! Guest interrupt state belongs to one VM-local [`arm_vgic::VgicCore`];
//! host IRQ tokens remain opaque until that controller completes split EOI.

use std::sync::Arc;

use arm_vcpu::*;
use arm_vgic::{GicV3VcpuBinding, IntId, VgicCore};
use ax_memory_addr::VirtAddr;
use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult, *};

use super::*;
use crate::{AxVmResult, ax_err};

mod capabilities;
#[path = "../../architecture/cpu_up.rs"]
mod cpu_up;
pub(crate) mod fdt;
mod firmware_plan;
mod gic;
mod images;
mod npt;
mod resource_pools;
mod shared_mmio;
mod shared_provider;
#[path = "../../architecture/sysreg.rs"]
mod sysreg;
mod vgic;
mod vm;
mod vm_plan;
pub(crate) use vm_plan::Aarch64VmPlan;
mod vtimer;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
use cpu_up::{CpuUpExit, CpuUpOps};
pub use images::ImageLoader;
use sysreg::{SysRegReadExit, SysRegWriteExit};
use vgic::Aarch64VgicRuntimeKey;

pub(crate) struct Aarch64Arch;

#[derive(Clone, Copy, Debug)]
pub(crate) enum Aarch64DeferredRunWork {
    ExternalInterrupt { token: Option<usize> },
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

    fn activate_devices(vm: &crate::AxVM) -> AxVmResult {
        vgic_runtime(vm)?.activate()
    }

    fn deactivate_devices(vm: &crate::AxVM) -> AxVmResult {
        vgic_runtime(vm)?.deactivate()
    }

    fn before_vcpu_run(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        vcpu.get_arch_vcpu().prepare_timer_run()
    }

    fn on_last_vcpu_exit(vm: &crate::AxVMRef) -> AxVmResult {
        Self::deactivate_devices(vm)
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
            ArmVmExit::Hypercall { nr, args } => super::handle_hypercall(
                vm,
                vcpu,
                HypercallExit { nr, args },
                crate::runtime::hvc::HyperCallAbi::AArch64,
            ),
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
            ArmVmExit::MmioWrite { addr, width, data } => super::handle_mmio_write::<Self>(
                vm,
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
            ArmVmExit::GicCpuInterfaceRead {
                register,
                destination,
            } => {
                let value = vcpu.get_arch_vcpu().read_icc(register)?;
                vcpu.set_gpr(destination, value as usize);
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::GicCpuInterfaceWrite { register, value } => {
                vcpu.get_arch_vcpu().write_icc(register, value)?;
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::ExternalInterrupt { token } => Ok(BoundVcpuExit::Defer(
                Aarch64DeferredRunWork::ExternalInterrupt { token },
            )),
            ArmVmExit::WaitForInterrupt => {
                vcpu.get_arch_vcpu().arm_timer_wait()?;
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: true,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            ArmVmExit::CpuDown { state } => {
                warn!(
                    "VM[{}] run VCpu[{}] CpuDown state {state:#x}",
                    vm.id(),
                    vcpu.id()
                );
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: true,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                }))
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
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: Some(crate::StopReason::SystemDown),
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            ArmVmExit::SendIPI { value } => {
                vcpu.get_arch_vcpu().write_sgi1r(value)?;
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::DeactivateInterrupt { intid } => {
                vcpu.get_arch_vcpu().deactivate(intid)?;
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::Nothing => Ok(BoundVcpuExit::Complete(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
                resets_vm: false,
                exits_vcpu: false,
            })),
            _ => ax_err!(Unsupported, "unsupported AArch64 VM exit"),
        }
    }

    fn finish_deferred_run_work(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction> {
        match work {
            Aarch64DeferredRunWork::ExternalInterrupt { token } => {
                if let Some(token) = token {
                    if !vcpu.get_arch_vcpu().accept_host_timer_irq(token) {
                        gic::route_acknowledged_host_irq(token).map_err(|error| {
                            crate::AxVmError::interrupt("route acknowledged host IRQ", error)
                        })?;
                    }
                }
                crate::check_timer_events();
            }
        }
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
            resets_vm: false,
            exits_vcpu: false,
        })
    }

    fn wait_for_vcpu_event(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        runtime: &crate::vm::VmRuntimeHandle,
    ) {
        runtime.wait_until(|| {
            if !vm.running() {
                return true;
            }
            match vcpu.get_arch_vcpu().has_pending_interrupt() {
                Ok(true) => true,
                Ok(false) => match vcpu.get_arch_vcpu().arm_timer_wait() {
                    Ok(()) => match vcpu.get_arch_vcpu().has_pending_interrupt() {
                        Ok(pending) => pending,
                        Err(error) => {
                            warn!(
                                "VM[{}] VCpu[{}] cannot recheck VGIC after arming timer: {error:?}",
                                vm.id(),
                                vcpu.id()
                            );
                            true
                        }
                    },
                    Err(error) => {
                        warn!(
                            "VM[{}] VCpu[{}] cannot rearm architectural timer before WFI wait: \
                             {error:?}",
                            vm.id(),
                            vcpu.id()
                        );
                        true
                    }
                },
                Err(error) => {
                    warn!(
                        "VM[{}] VCpu[{}] cannot query VGIC pending state before WFI wait: \
                         {error:?}",
                        vm.id(),
                        vcpu.id()
                    );
                    true
                }
            }
        });
    }
}

fn vgic_runtime(vm: &crate::AxVM) -> AxVmResult<Arc<vgic::Aarch64VgicRuntime>> {
    Ok(vm
        .get_devices()?
        .services()
        .require::<Aarch64VgicRuntimeKey>()?)
}

struct AxvmArmHostOps;

impl ArmHostOps for AxvmArmHostOps {
    fn inject_virtual_interrupt(_vector: u32) -> ArmVcpuResult {
        Err(ArmVcpuError::Unsupported)
    }

    fn finish_pending_host_irq(raw_ack: u32) -> Option<usize> {
        gic::finish_pending_host_irq(raw_ack)
    }

    fn handle_current_host_irq() {
        if let Some(token) = gic::acknowledge_host_irq()
            && let Err(error) = gic::route_acknowledged_host_irq(token)
        {
            warn!("{error}");
        }
        crate::check_timer_events();
    }
}

pub(crate) struct AxvmArmVcpu {
    vm_id: usize,
    inner: ArmVcpu<AxvmArmHostOps>,
    vgic: Option<Arc<VgicCore>>,
    vgic_binding: Option<GicV3VcpuBinding>,
    timer_binding: Option<Arc<vtimer::Aarch64TimerBinding>>,
}

impl AxvmArmVcpu {
    pub(crate) fn attach_vgic(
        &mut self,
        vgic: Arc<VgicCore>,
        irq_binding: vgic::Aarch64VcpuIrqBinding,
        timer_config: arm_vcpu::ArmTimerVmConfig,
    ) -> AxVmResult {
        if self.vgic_binding.is_some() {
            return ax_err!(BadState, "AArch64 vCPU already has a VGIC binding");
        }
        let vgic::Aarch64VcpuIrqBinding {
            gic: binding,
            backend,
            virtual_timer_ppi,
            physical_timer_ppi,
            host_virtual_timer_intid,
        } = irq_binding;
        let timer_binding = vtimer::Aarch64TimerBinding::new(
            self.vm_id,
            vgic.clone(),
            backend,
            binding.vcpu(),
            virtual_timer_ppi,
            physical_timer_ppi,
            host_virtual_timer_intid,
            timer_config.frequency(),
        )
        .map_err(|error| crate::AxVmError::interrupt("bind host virtual-timer PPI", error))?;
        self.vgic = Some(vgic);
        self.vgic_binding = Some(binding);
        self.timer_binding = Some(timer_binding);
        Ok(())
    }

    fn binding(&self) -> AxVmResult<&GicV3VcpuBinding> {
        self.vgic_binding
            .as_ref()
            .ok_or_else(|| crate::AxVmError::resource_unavailable("VGIC vCPU binding", "missing"))
    }

    fn write_sgi1r(&self, value: u64) -> AxVmResult {
        self.binding()?
            .write_sgi1r(value)
            .map_err(|error| crate::AxVmError::interrupt("write ICC_SGI1R_EL1", error))
    }

    fn read_icc(&self, register: ArmGicCpuInterfaceRegister) -> AxVmResult<u64> {
        let binding = self.binding()?;
        let result = match register {
            ArmGicCpuInterfaceRegister::Control => binding.read_icc_control(),
            ArmGicCpuInterfaceRegister::PriorityMask => binding.read_icc_priority_mask(),
            ArmGicCpuInterfaceRegister::RunningPriority => binding.read_icc_running_priority(),
        };
        result.map_err(|error| crate::AxVmError::interrupt("read virtual ICC register", error))
    }

    fn write_icc(&self, register: ArmGicCpuInterfaceRegister, value: u64) -> AxVmResult {
        let binding = self.binding()?;
        let result = match register {
            ArmGicCpuInterfaceRegister::Control => binding.write_icc_control(value),
            ArmGicCpuInterfaceRegister::PriorityMask => binding.write_icc_priority_mask(value),
            ArmGicCpuInterfaceRegister::RunningPriority => Ok(()),
        };
        result.map_err(|error| crate::AxVmError::interrupt("write virtual ICC register", error))
    }

    fn deactivate(&self, intid: u32) -> AxVmResult {
        let intid = IntId::new(intid)
            .map_err(|error| crate::AxVmError::interrupt("validate ICC_DIR_EL1 INTID", error))?;
        self.binding()?
            .deactivate_saved(intid)
            .map_err(|error| crate::AxVmError::interrupt("deactivate virtual interrupt", error))
    }

    fn synchronize_timer(&self) -> BackendResult {
        let snapshot = arm_result(self.inner.timer_snapshot())?;
        let binding = self
            .timer_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?;
        vgic_backend_result(binding.synchronize(snapshot))
    }

    fn arm_timer_wait(&self) -> AxVmResult {
        let snapshot = self.inner.timer_snapshot().map_err(|error| {
            crate::AxVmError::vcpu(
                "snapshot AArch64 architectural timers",
                std::format!("{error:?}"),
            )
        })?;
        self.timer_binding
            .as_ref()
            .ok_or_else(|| {
                crate::AxVmError::resource_unavailable("AArch64 timer binding", "missing")
            })?
            .arm_wait(snapshot)
            .map_err(|error| crate::AxVmError::interrupt("arm architectural timer wait", error))
    }

    fn invalidate_virtual_timer_wait(&self) {
        if let Some(binding) = &self.timer_binding {
            binding.invalidate_wait();
        }
    }

    fn prepare_timer_run(&self) -> AxVmResult {
        self.invalidate_virtual_timer_wait();
        self.timer_binding
            .as_ref()
            .ok_or_else(|| {
                crate::AxVmError::resource_unavailable("AArch64 timer binding", "missing")
            })?
            .prepare_run()
            .map_err(|error| crate::AxVmError::interrupt("prepare timer PPI route", error))
    }

    fn accept_host_timer_irq(&self, token: usize) -> bool {
        self.timer_binding
            .as_ref()
            .is_some_and(|binding| binding.accept_host_irq(token))
    }

    fn has_pending_interrupt(&self) -> AxVmResult<bool> {
        self.binding()?
            .has_pending_interrupt()
            .map_err(|error| crate::AxVmError::interrupt("query pending virtual interrupt", error))
    }
}

impl VmArchVcpuOps for AxvmArmVcpu {
    type CreateConfig = ArmVcpuCreateConfig;
    type SetupConfig = ArmVcpuSetupConfig;
    type Exit = ArmVmExit;

    fn guest_mpidr_from_create_config(config: &Self::CreateConfig) -> Option<u64> {
        Some(config.mpidr_el1 as u64)
    }

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        arm_result(ArmVcpu::new(vm_id, vcpu_id, config)).map(|inner| Self {
            vm_id,
            inner,
            vgic: None,
            vgic_binding: None,
            timer_binding: None,
        })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        arm_result(self.inner.set_entry(ax_guest_phys_addr_to_arm(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        arm_result(
            self.inner
                .set_nested_page_table(ax_nested_paging_to_arm(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        arm_result(self.inner.setup(config))
    }

    fn run(&mut self) -> BackendResult<Self::Exit> {
        let binding = self
            .vgic_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?;
        let host_irq_guard = ArmHostIrqGuard::mask();
        vgic_backend_result(binding.load())?;
        let run_result = arm_result(self.inner.run(&host_irq_guard));
        let timer_result = self.synchronize_timer();
        let save_result = vgic_backend_result(binding.save());
        drop(host_irq_guard);
        match run_result {
            Ok(exit) => {
                timer_result?;
                save_result?;
                Ok(exit)
            }
            Err(error) => {
                if let Err(save_error) = save_result {
                    warn!("failed to save VGIC state after vCPU run error: {save_error}");
                }
                Err(error)
            }
        }
    }

    fn bind(&mut self) -> BackendResult {
        arm_result(self.inner.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        arm_result(self.inner.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.inner.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        self.inject_interrupt_with_trigger(vector, InterruptTriggerMode::EdgeTriggered)
    }

    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> BackendResult {
        let vgic = self.vgic.as_ref().ok_or(BackendError::InvalidState)?;
        let binding = self
            .vgic_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?;
        let vector = u32::try_from(vector).map_err(|_| BackendError::InvalidInput)?;
        vgic_backend_result(vgic.inject(binding.vcpu().raw(), vector, trigger))
    }

    fn set_return_value(&mut self, val: usize) {
        self.inner.set_return_value(val);
    }
}

impl Drop for AxvmArmVcpu {
    fn drop(&mut self) {
        if let Some(binding) = &self.timer_binding
            && let Err(error) = binding.reset()
        {
            warn!("failed to reset AArch64 timer binding while dropping vCPU: {error}");
        }
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
        arm_result(self.0.hardware_enable::<AxvmArmHostOps>())?;
        if let Err(error) = gic::enable_maintenance_interrupt() {
            if let Err(rollback_error) = self.0.hardware_disable() {
                warn!(
                    "failed to roll back AArch64 virtualization after maintenance IRQ setup \
                     failed: {rollback_error:?}"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    fn hardware_disable(&mut self) -> BackendResult {
        gic::disable_maintenance_interrupt()?;
        arm_result(self.0.hardware_disable())
    }

    fn max_guest_page_table_levels(&self) -> usize {
        self.0.max_guest_page_table_levels()
    }

    fn guest_phys_addr_bits(&self) -> usize {
        self.0.guest_phys_addr_bits()
    }

    fn timer_frequency_hz(&self) -> Option<u64> {
        Some(self.0.timer_frequency_hz())
    }
}

fn arm_result<T>(result: ArmVcpuResult<T>) -> BackendResult<T> {
    result.map_err(arm_error_to_backend)
}

fn vgic_backend_result<T>(result: arm_vgic::VgicResult<T>) -> BackendResult<T> {
    result.map_err(|error| match error {
        arm_vgic::VgicError::InvalidIntId { .. }
        | arm_vgic::VgicError::WrongIntIdClass { .. }
        | arm_vgic::VgicError::InvalidConfig { .. } => BackendError::InvalidInput,
        arm_vgic::VgicError::InvalidAccess { .. }
        | arm_vgic::VgicError::InvalidItsCommand { .. }
        | arm_vgic::VgicError::ItsCommandBudgetExceeded { .. } => BackendError::InvalidData,
        arm_vgic::VgicError::ResourceConflict { .. } => BackendError::ResourceBusy,
        arm_vgic::VgicError::DeliveryQueueFull { .. } => BackendError::OutOfMemory,
        arm_vgic::VgicError::Unsupported { .. } => BackendError::Unsupported,
        arm_vgic::VgicError::ResourceNotFound { .. }
        | arm_vgic::VgicError::InvalidStateTransition { .. }
        | arm_vgic::VgicError::Backend { .. }
        | arm_vgic::VgicError::GuestMemory { .. } => BackendError::InvalidState,
    })
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

    #[test]
    fn guest_mpidr_from_real_arm_create_config() {
        let config = ArmVcpuCreateConfig {
            mpidr_el1: 0x100,
            dtb_addr: 0x4000_0000,
        };

        assert_eq!(
            <AxvmArmVcpu as VmArchVcpuOps>::guest_mpidr_from_create_config(&config),
            Some(0x100),
        );
    }
}
