//! AxVM AArch64 adapter.
//!
//! This module owns the AxVM/ArceOS glue for the OS-neutral `arm_vcpu` core:
//! `AxvmArmHostOps` supplies host IRQ/GIC operations, while this module handles
//! `arm_vcpu` exits inside the AArch64 architecture boundary.

use alloc::boxed::Box;
use core::time::Duration;

use arm_vcpu::{
    ArmAccessWidth, ArmGuestPhysAddr, ArmHostIrq, ArmHostIrqOwnership, ArmHostOps,
    ArmNestedPagingConfig, ArmPerCpu, ArmSysRegAddr, ArmVcpu, ArmVcpuCreateConfig, ArmVcpuError,
    ArmVcpuResult, ArmVcpuSetupConfig, ArmVirtualIntId, ArmVmExit,
};
use arm_vgic::host::ArmVgicHostIf;
use ax_crate_interface::impl_interface;
use ax_memory_addr::{PhysAddr, VirtAddr};
use axvm_types::{
    AccessWidth, GuestPhysAddr, InterruptTriggerMode, NestedPagingConfig, SysRegAddr, VCpuId, VMId,
    VmArchPerCpuOps, VmArchVcpuOps, VmBackendError as BackendError,
    VmBackendResult as BackendResult,
};

use super::{ArchOps, BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction};
use crate::{
    AxVmResult, ax_err,
    host::{HostCpu, HostMemory, HostTime, default_host},
};

mod capabilities;
#[path = "../../architecture/cpu_up.rs"]
mod cpu_up;
pub(crate) mod fdt;
mod gic;
mod gicv2;
mod images;
mod ipi;
mod maintenance;
mod maintenance_registration;
mod maintenance_state;
mod npt;
#[path = "../../architecture/sysreg.rs"]
mod sysreg;
mod vgic;
mod vm;
mod vtimer;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
use cpu_up::{CpuUpExit, CpuUpOps};
pub use images::ImageLoader;
use ipi::SendIpiExit;
use sysreg::{SysRegReadExit, SysRegWriteExit};
use vgic::DirOutcome;

pub(crate) struct Aarch64Arch;

const ICC_DIR_EL1: ArmSysRegAddr = ArmSysRegAddr::new(0x32_3016);

#[derive(Clone, Copy, Debug)]
pub(crate) enum Aarch64DeferredRunWork {
    ExternalInterrupt { host_irq: ArmHostIrq },
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

    fn register_platform_irq_injector() {
        let _ = maintenance::register_handler();
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
            ArmVmExit::SysRegWrite { addr, value } if addr == ICC_DIR_EL1 => {
                vcpu.get_arch_vcpu()
                    .handle_dir(value)
                    .map_err(|error| match error {
                        BackendError::Unsupported => crate::AxVmError::unsupported(
                            "deactivate virtual interrupt",
                            "the DIR target cannot be serviced by this vCPU",
                        ),
                        _ => crate::AxVmError::vcpu(
                            "deactivate virtual interrupt",
                            format_args!("{error:?}"),
                        ),
                    })?;
                Ok(BoundVcpuExit::Continue)
            }
            ArmVmExit::SysRegWrite { addr, value } => sysreg::handle_write(
                vm,
                SysRegWriteExit {
                    addr: arm_sys_reg_addr_to_ax(addr),
                    value,
                },
            ),
            ArmVmExit::ExternalInterrupt { host_irq } => {
                debug!(
                    "VM[{}] run VCpu[{}] get irq {}",
                    vm.id(),
                    vcpu.id(),
                    host_irq.vector()
                );
                Ok(BoundVcpuExit::Defer(
                    Aarch64DeferredRunWork::ExternalInterrupt { host_irq },
                ))
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
                }))
            }
            ArmVmExit::SendIPI {
                target_cpu,
                target_cpu_aux,
                send_to_all,
                send_to_self,
                vector,
            } => ipi::handle(
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
            ArmVmExit::Nothing => Ok(BoundVcpuExit::Complete(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
            })),
            _ => ax_err!(Unsupported, "unsupported AArch64 VM exit"),
        }
    }

    fn finish_deferred_run_work(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction> {
        match work {
            Aarch64DeferredRunWork::ExternalInterrupt { host_irq } => match host_irq.ownership() {
                ArmHostIrqOwnership::FetchHandled => {
                    crate::check_timer_events();
                }
                ArmHostIrqOwnership::DeferredDispatch => {
                    Self::after_external_interrupt(_vm, _vcpu, host_irq.vector());
                }
            },
        }
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
    }
}

struct AxvmArmHostOps;

impl ArmHostOps for AxvmArmHostOps {
    fn interrupt_virtualization() -> ArmVcpuResult<arm_vcpu::ArmInterruptVirtualization> {
        gic::interrupt_virtualization()
    }

    fn current_cpu_id() -> ArmVcpuResult<usize> {
        Ok(default_host().this_cpu_id())
    }

    fn inject_virtual_interrupt(intid: ArmVirtualIntId) -> ArmVcpuResult {
        gic::inject_interrupt(intid)
    }

    fn fetch_pending_host_irq() -> Option<ArmHostIrq> {
        Some(gic::fetch_irq())
    }

    fn handle_current_host_irq() {
        gic::handle_current_irq();
    }
}

pub(crate) struct AxvmArmVcpu {
    backend: ArmVcpu<AxvmArmHostOps>,
    delivery: Option<vgic::ArmVgicDeliveryPort>,
    vm_id: VMId,
    vcpu_id: VCpuId,
    bind_generation: Option<u64>,
}

impl AxvmArmVcpu {
    fn with_exclusive_ich_access<T>(
        &mut self,
        operation: impl FnOnce(&mut ArmVcpu<AxvmArmHostOps>) -> T,
    ) -> T {
        let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
        // SAFETY: AxVM only reaches direct hardware injection from the owning
        // vCPU run path; remote producers enqueue work. The IRQ guard excludes
        // local re-entry, and no ICH register has a remotely addressable alias.
        unsafe {
            ax_percpu::with_cpu_pin(|pin| {
                assert_eq!(
                    pin.area().cpu_index().as_usize(),
                    default_host().this_cpu_id(),
                    "host CPU identity disagrees with the pinned CPU area"
                );
                ax_percpu::with_exclusive_cpu(pin, |_| operation(&mut self.backend))
            })
        }
        .expect("ICH access requires an installed CPU-local area")
    }
}

impl VmArchVcpuOps for AxvmArmVcpu {
    type CreateConfig = ArmVcpuCreateConfig;
    type SetupConfig = ArmVcpuSetupConfig;
    type Exit = ArmVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        arm_result(ArmVcpu::new(vm_id, vcpu_id, config)).map(|backend| Self {
            backend,
            delivery: None,
            vm_id,
            vcpu_id,
            bind_generation: None,
        })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        arm_result(self.backend.set_entry(ax_guest_phys_addr_to_arm(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        arm_result(
            self.backend
                .set_nested_page_table(ax_nested_paging_to_arm(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        arm_result(self.backend.setup(config))
    }

    fn run(&mut self) -> BackendResult<Self::Exit> {
        self.service_emulated_spis(false)?;
        let exit = arm_result(self.backend.run())?;
        let maintenance_seen = self.consume_maintenance_observation()?;
        self.service_emulated_spis(maintenance_seen)?;
        Ok(exit)
    }

    fn bind(&mut self) -> BackendResult {
        if self.delivery.is_none() {
            return arm_result(self.backend.bind());
        }
        let generation = maintenance::next_generation()?;
        let cpu_id = default_host().this_cpu_id();
        maintenance::publish(cpu_id, self.vm_id, self.vcpu_id, generation)?;
        if let Err(error) = arm_result(self.backend.bind()) {
            let _ = maintenance::withdraw(cpu_id, self.vm_id, self.vcpu_id, generation);
            return Err(error);
        }
        self.bind_generation = Some(generation);
        Ok(())
    }

    fn unbind(&mut self) -> BackendResult {
        if self.delivery.is_none() {
            return arm_result(self.backend.unbind());
        }
        let service_result = self
            .consume_maintenance_observation()
            .and_then(|maintenance_seen| self.service_emulated_spis_bound(maintenance_seen));
        let unbind_result = arm_result(self.backend.unbind());
        let withdraw_result = if let Some(generation) = self.bind_generation.take() {
            maintenance::withdraw(
                default_host().this_cpu_id(),
                self.vm_id,
                self.vcpu_id,
                generation,
            )
        } else {
            Ok(())
        };
        service_result.and(unbind_result).and(withdraw_result)
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.backend.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        self.with_exclusive_ich_access(|vcpu| arm_result(vcpu.inject_interrupt(vector)))
    }

    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> BackendResult {
        // The Arm Router/VGIC consumes line trigger semantics before emitting
        // an INTID. The GIC list-register injection itself is mode-agnostic.
        match trigger {
            InterruptTriggerMode::EdgeTriggered | InterruptTriggerMode::LevelTriggered => {
                self.with_exclusive_ich_access(|vcpu| arm_result(vcpu.inject_interrupt(vector)))
            }
        }
    }

    fn set_return_value(&mut self, val: usize) {
        self.backend.set_return_value(val);
    }
}

impl AxvmArmVcpu {
    fn consume_maintenance_observation(&self) -> BackendResult<bool> {
        let Some(generation) = self.bind_generation else {
            return Ok(false);
        };
        let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
        maintenance::consume(
            default_host().this_cpu_id(),
            self.vm_id,
            self.vcpu_id,
            generation,
        )
    }

    fn service_emulated_spis(&mut self, read_maintenance: bool) -> BackendResult {
        let Some(delivery) = self.delivery.as_mut() else {
            return Ok(());
        };
        let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
        let backend = &mut self.backend;
        // SAFETY: the vCPU transition owns the backend. IRQ exclusion prevents
        // local ICH re-entry and the pin proves this operation cannot migrate.
        unsafe {
            ax_percpu::with_cpu_pin(|pin| {
                ax_percpu::with_exclusive_cpu(pin, |_| {
                    service_delivery(backend, delivery, read_maintenance)
                })
            })
        }
        .map_err(|_| BackendError::InvalidState)?
    }

    /// Services the delivery port while the caller already owns the current CPU.
    fn service_emulated_spis_bound(&mut self, read_maintenance: bool) -> BackendResult {
        let Some(delivery) = self.delivery.as_mut() else {
            return Ok(());
        };
        service_delivery(&mut self.backend, delivery, read_maintenance)
    }

    fn handle_dir(&mut self, value: u64) -> BackendResult {
        let raw_intid = usize::try_from(value).map_err(|_| BackendError::InvalidInput)?;
        let intid = ArmVirtualIntId::try_from(raw_intid).map_err(|_| BackendError::InvalidInput)?;
        let _guard = ax_kernel_guard::NoPreemptIrqSave::new();
        let backend = &mut self.backend;
        let delivery = &mut self.delivery;
        let mut delivery_error = None;
        let result = unsafe {
            ax_percpu::with_cpu_pin(|pin| {
                ax_percpu::with_exclusive_cpu(pin, |_| {
                    backend.with_bound_ich(|session| {
                        let handled = if let Some(port) = delivery.as_mut() {
                            match port.handle_dir(session, intid) {
                                Ok(DirOutcome::Completed | DirOutcome::Compatibility) => Ok(()),
                                Ok(DirOutcome::ServiceTarget(_)) => Err(BackendError::Unsupported),
                                Err(error) => Err(error),
                            }
                        } else {
                            match session.deactivate_compatibility_interrupt(intid) {
                                Ok(true) => Ok(()),
                                Ok(false) => Err(BackendError::Unsupported),
                                Err(_) => Err(BackendError::InvalidState),
                            }
                        };
                        if let Err(error) = handled {
                            delivery_error = Some(error);
                            return Err(ArmVcpuError::BadState);
                        }
                        Ok(())
                    })
                })
            })
        }
        .map_err(|_| BackendError::InvalidState)?;
        if let Some(error) = delivery_error {
            return Err(error);
        }
        arm_result(result)
    }
}

fn service_delivery(
    backend: &mut ArmVcpu<AxvmArmHostOps>,
    delivery: &mut vgic::ArmVgicDeliveryPort,
    read_maintenance: bool,
) -> BackendResult {
    let mut delivery_error = None;
    let result = backend.with_bound_ich(|session| {
        if let Err(error) = delivery.service(session, read_maintenance) {
            delivery_error = Some(error);
            return Err(ArmVcpuError::BadState);
        }
        Ok(())
    });
    if let Some(error) = delivery_error {
        return Err(error);
    }
    arm_result(result)
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
        ArmVcpuError::BadState
        | ArmVcpuError::IchVcpuAlreadyBound { .. }
        | ArmVcpuError::IchVcpuNotBound
        | ArmVcpuError::IchVcpuCpuMismatch { .. } => BackendError::InvalidState,
        ArmVcpuError::InvalidVirtualInterruptId { .. } => BackendError::InvalidInput,
        ArmVcpuError::InvalidListRegisterCount { .. }
        | ArmVcpuError::MalformedListRegister { .. }
        | ArmVcpuError::InvalidIchCapability { .. }
        | ArmVcpuError::UnexpectedIchHcrBits { .. }
        | ArmVcpuError::UnexpectedIchEoiCount { .. } => BackendError::InvalidData,
        ArmVcpuError::UnknownIchMaintenanceReasons { .. } | ArmVcpuError::InvalidIchEisr { .. } => {
            BackendError::InvalidData
        }
        ArmVcpuError::NoFreeListRegister { .. } => BackendError::ResourceBusy,
        ArmVcpuError::UnsupportedListRegister { .. }
        | ArmVcpuError::UnsupportedIchHcrPolicy { .. }
        | ArmVcpuError::IncompatibleIchCapabilities { .. }
        | ArmVcpuError::IncompatibleIchVcpuCapability { .. } => BackendError::Unsupported,
        ArmVcpuError::IchCapabilityCpuOutOfRange { .. } => BackendError::InvalidInput,
        ArmVcpuError::IchCapabilityNotPublished { .. }
        | ArmVcpuError::IchCapabilityConflict { .. }
        | ArmVcpuError::IchRegisterAccess { .. } => BackendError::InvalidState,
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
        assert_eq!(
            arm_error_to_backend(ArmVcpuError::InvalidVirtualInterruptId { value: 1020 }),
            BackendError::InvalidInput
        );
        assert_eq!(
            arm_error_to_backend(ArmVcpuError::MalformedListRegister { slot: 0 }),
            BackendError::InvalidData
        );
        assert_eq!(
            arm_error_to_backend(ArmVcpuError::NoFreeListRegister {
                intid: ArmVirtualIntId::new(32).unwrap(),
            }),
            BackendError::ResourceBusy
        );
        assert_eq!(
            arm_error_to_backend(ArmVcpuError::UnsupportedListRegister { slot: 0 }),
            BackendError::Unsupported
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

    fn current_vm_id() -> usize {
        crate::current_vm_id().expect("current AArch64 VM is not set")
    }

    fn queue_virtual_interrupt(vm_id: usize, vcpu_id: usize, vector: u8) {
        if let Err(err) = crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector as usize) {
            warn!(
                "failed to queue VM[{vm_id}] vCPU[{vcpu_id}] virtual interrupt {vector}: {err:?}"
            );
        }
    }

    fn current_time_nanos() -> u64 {
        default_host().monotonic_time().as_nanos() as u64
    }

    fn register_timer(
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> usize {
        crate::timer::register_timer(deadline.as_nanos() as u64, callback)
    }

    fn cancel_timer(token: usize) {
        crate::timer::cancel_timer(token);
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
        if let Err(err) = crate::manager::inject_current_vcpu_interrupt(usize::from(vector)) {
            warn!("failed to inject private virtual interrupt {vector}: {err}");
        }
    }
}
