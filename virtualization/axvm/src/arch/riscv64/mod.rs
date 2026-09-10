use std::sync::Arc;

use ax_memory_addr::VirtAddr;
use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult, *};

use self::policy::{GprIndex as RiscvGprIndex, *};
use super::*;
use crate::{AxVmResult, StopReason, architecture::ops::*, host::*};

mod capabilities;
pub(crate) mod fdt;
mod hsm;
mod ipi;
mod irq;
mod npt;
mod policy;
mod resource_pools;
mod vm;
pub(crate) use vm::RiscvVmPlan;

pub(crate) struct Riscv64Arch;

#[derive(Clone, Copy, Debug)]
pub(crate) enum RiscvDeferredRunWork {
    ExternalInterrupt { vector: usize },
}

impl ArchOps for Riscv64Arch {
    type VCpu = AxvmRiscvVcpu;
    type PerCpu = AxvmRiscvPerCpu;
    type DeferredRunWork = RiscvDeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>, vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, vcpu_id);
        vcpu.set_gpr(RiscvGprIndex::A1 as usize, arg);
    }

    fn has_hardware_support() -> bool {
        ax_cpu::capability::has_hypervisor_extension()
    }

    fn enter_runtime(vm: &crate::AxVM) -> AxVmResult {
        vplic_runtime(vm)?.activate()
    }

    fn exit_runtime(vm: &crate::AxVM) -> AxVmResult {
        vplic_runtime(vm)?.deactivate()
    }

    fn before_vcpu_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) -> AxVmResult {
        sync_vplic_vseip(vm, vcpu)
    }

    fn inject_vcpu_interrupt(
        vcpu: &crate::vcpu::AxVCpu<Self::VCpu>,
        interrupt: crate::irq::model::PendingVcpuInterrupt,
    ) -> AxVmResult {
        const SCAUSE_INTERRUPT_BIT: usize = 1 << (usize::BITS - 1);

        // VirtualInterruptId carries the RISC-V cause number. The backend
        // consumes a complete scause value, including its interrupt bit.
        let vector = SCAUSE_INTERRUPT_BIT | interrupt.id.0 as usize;
        vcpu.inject_interrupt_with_trigger(vector, interrupt.trigger)
    }

    fn after_vcpu_run(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: RiscvVmExit,
    ) -> AxVmResult<RiscvVmExit> {
        match exit {
            RiscvVmExit::Machine(exit) => riscv_result(vcpu.get_arch_vcpu().0.process_exit(exit))
                .map_err(|error| {
                    crate::vcpu::map_vcpu_backend_error("interpret RISC-V exit", error)
                }),
            exit => Ok(exit),
        }
    }

    fn handle_vcpu_exit_unbound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
            RiscvVmExit::Machine(_) => {
                ax_err!(BadState, "CPU exit was not interpreted before unloading")
            }
            RiscvVmExit::Hypercall { nr, args } => super::handle_hypercall(
                vm,
                vcpu,
                HypercallExit { nr, args },
                crate::runtime::hvc::HyperCallAbi::Generic,
            ),
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
            RiscvVmExit::MmioWrite { addr, width, data } => super::handle_mmio_write(
                vm,
                vcpu,
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
            RiscvVmExit::SendIpi(request) => ipi::handle(vm, vcpu, request),
            RiscvVmExit::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => hsm::handle(
                vm,
                vcpu,
                hsm::HartStart {
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
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            RiscvVmExit::Halt => {
                debug!("VM[{}] run VCpu[{}] Halt", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: true,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            RiscvVmExit::SystemDown => {
                warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: Some(StopReason::SystemDown),
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            RiscvVmExit::Nothing => Ok(BoundVcpuExit::Complete(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
                resets_vm: false,
                exits_vcpu: false,
            })),
        }
    }

    fn finish_deferred_run_work(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction> {
        match work {
            RiscvDeferredRunWork::ExternalInterrupt { vector } => {
                finish_external_interrupt(vcpu, vector);
            }
        }
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
            resets_vm: false,
            exits_vcpu: false,
        })
    }

    fn on_last_vcpu_exit(vm: &crate::AxVMRef) -> AxVmResult {
        Self::exit_runtime(vm)
    }
}

fn finish_external_interrupt(vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>, vector: usize) {
    vcpu.with_current_cpu_set(|| {
        crate::host::arceos::dispatch_host_irq(vector);
        vcpu.get_arch_vcpu().latch_hvip_from_hw();
    });
}

fn vplic_runtime(vm: &crate::AxVM) -> AxVmResult<Arc<irq::RiscvPlicRuntime>> {
    vm.get_devices()?
        .services()
        .require::<irq::RiscvPlicRuntimeKey>()
        .map_err(Into::into)
}

fn sync_vplic_vseip(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>) -> AxVmResult {
    let asserted = vplic_runtime(vm)?.vcpu_has_deliverable_irq(vcpu.id())?;
    vcpu.get_arch_vcpu().set_vseip_level(asserted);
    Ok(())
}

fn handle_riscv_nested_page_fault(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    addr: RiscvGuestPhysAddr,
    access_flags: RiscvAccessFlags,
) -> AxVmResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let ax_addr = riscv_guest_phys_addr_to_ax(addr);
    let decoded = vcpu.with_backend_bound_current_cpu(|| {
        Ok(vcpu.get_arch_vcpu().decode_mmio_fault(addr, access_flags))
    })?;
    if let Some(decoded) = decoded {
        let handled = match decoded {
            RiscvVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => super::try_handle_mmio_read(
                vm,
                vcpu,
                MmioReadExit {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    width: riscv_access_width_to_ax(width),
                    reg,
                    reg_width: riscv_access_width_to_ax(reg_width),
                    signed_ext,
                },
            )?,
            RiscvVmExit::MmioWrite { addr, width, data } => super::try_handle_mmio_write(
                vm,
                vcpu,
                MmioWriteExit {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    width: riscv_access_width_to_ax(width),
                    data,
                },
            )?,
            _ => false,
        };
        if handled {
            return Ok(BoundVcpuExit::Continue);
        }
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
            resets_vm: false,
            exits_vcpu: false,
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

pub(crate) struct AxvmRiscvVcpu(RiscvVcpu<AxvmRiscvHostOps>);

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

    fn set_vseip_level(&mut self, asserted: bool) {
        self.0.set_vseip_level(asserted);
    }

    fn complete_ipi(&mut self, request: RiscvIpiRequest, completion: RiscvIpiCompletion) {
        self.0.complete_ipi(request, completion);
    }
}

impl VmArchVcpuOps for AxvmRiscvVcpu {
    type CreateConfig = RiscvVcpuCreateConfig;
    type SetupConfig = ();
    type Exit = RiscvVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        riscv_result(RiscvVcpu::new(vm_id, vcpu_id, config)).map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        riscv_result(self.0.set_entry(ax_guest_phys_addr_to_riscv(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        riscv_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_riscv(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        riscv_result(self.0.setup(config))
    }

    fn run(&mut self) -> BackendResult<Self::Exit> {
        riscv_result(self.0.run_machine()).map(RiscvVmExit::Machine)
    }

    fn bind(&mut self) -> BackendResult {
        riscv_result(self.0.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        riscv_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        riscv_result(self.0.inject_interrupt(vector))
    }

    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> BackendResult {
        // The vPLIC Router consumes source trigger semantics before setting a
        // virtual pending bit. The vCPU injection operation is mode-agnostic.
        match trigger {
            InterruptTriggerMode::EdgeTriggered | InterruptTriggerMode::LevelTriggered => {
                riscv_result(self.0.inject_interrupt(vector))
            }
        }
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

pub(crate) struct AxvmRiscvPerCpu(ax_cpu::virtualization::PerCpu);

impl VmArchPerCpuOps for AxvmRiscvPerCpu {
    fn new(_cpu_id: usize) -> BackendResult<Self> {
        Ok(Self(ax_cpu::virtualization::PerCpu::new()))
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> BackendResult {
        // SAFETY: AxVM's current-percpu mutation holds its IRQ/preemption guard
        // and exclusively owns this hart before any vCPU can bind.
        unsafe { self.0.enable() }.map_err(cpu_virtualization_error)?;
        // Physical IRQ source ownership remains in the host integration.
        unsafe {
            ax_cpu::interrupt::enable_source(ax_cpu::interrupt::Interrupt::SupervisorExternal);
            ax_cpu::interrupt::enable_source(ax_cpu::interrupt::Interrupt::SupervisorSoft);
            ax_cpu::interrupt::enable_source(ax_cpu::interrupt::Interrupt::SupervisorTimer);
        }
        Ok(())
    }

    fn hardware_disable(&mut self) -> BackendResult {
        // SAFETY: AxVM retires the current per-CPU owner with all vCPUs unloaded.
        unsafe { self.0.disable() }.map_err(cpu_virtualization_error)
    }

    fn max_guest_page_table_levels(&self) -> usize {
        self.0.max_guest_page_table_levels()
    }

    fn guest_phys_addr_bits(&self) -> usize {
        self.0.guest_phys_addr_bits()
    }
}

fn riscv_result<T>(result: RiscvVcpuResult<T>) -> BackendResult<T> {
    result.map_err(riscv_error_to_backend)
}

fn riscv_error_to_backend(err: RiscvVcpuError) -> BackendError {
    match err {
        RiscvVcpuError::InvalidInput => BackendError::InvalidInput,
        RiscvVcpuError::Unsupported => BackendError::Unsupported,
        RiscvVcpuError::BadState => BackendError::InvalidState,
        RiscvVcpuError::InvalidTrap
        | RiscvVcpuError::DecodeFailed
        | RiscvVcpuError::GuestMemoryFault => BackendError::InvalidData,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_riscv_vcpu_errors_to_backend_errors() {
        assert_eq!(
            riscv_error_to_backend(RiscvVcpuError::InvalidInput),
            BackendError::InvalidInput
        );
        assert_eq!(
            riscv_error_to_backend(RiscvVcpuError::Unsupported),
            BackendError::Unsupported
        );
        assert_eq!(
            riscv_error_to_backend(RiscvVcpuError::BadState),
            BackendError::InvalidState
        );
        assert_eq!(
            riscv_error_to_backend(RiscvVcpuError::DecodeFailed),
            BackendError::InvalidData
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

fn cpu_virtualization_error(error: ax_cpu::virtualization::VirtualizationError) -> BackendError {
    use ax_cpu::virtualization::VirtualizationError;
    match error {
        VirtualizationError::InvalidRoot | VirtualizationError::InvalidVector => {
            BackendError::InvalidInput
        }
        VirtualizationError::Unavailable
        | VirtualizationError::UnsupportedPaging
        | VirtualizationError::UnsupportedTimer => BackendError::Unsupported,
        VirtualizationError::AlreadyEnabled | VirtualizationError::NotEnabled => {
            BackendError::InvalidState
        }
    }
}
