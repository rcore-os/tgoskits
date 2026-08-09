use std::{sync::Arc, vec::Vec};

use ax_memory_addr::VirtAddr;
use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult, *};
use riscv_vcpu::{GprIndex as RiscvGprIndex, *};

use super::*;
use crate::{AxVmResult, StopReason, architecture::ops::*, host::*};

mod capabilities;
#[path = "../../architecture/cpu_up.rs"]
mod cpu_up;
pub(crate) mod fdt;
mod images;
mod irq;
mod npt;
mod resource_pools;
mod vm;
pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
use cpu_up::{CpuUpExit, CpuUpOps};
pub use images::ImageLoader;
pub(crate) use vm::RiscvVmPlan;

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
    type DeferredRunWork = RiscvDeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

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
            targets = super::riscv_hart_mask_targets(
                target_cpu as usize,
                target_cpu_aux as usize,
                vm.get_vcpu_affinities_pcpu_ids(),
            );
        }

        targets
    }

    fn set_vcpu_on_args(vcpu: &crate::vm::AxVCpuRef<Self::VCpu>, vcpu_id: usize, arg: usize) {
        vcpu.set_gpr(RiscvGprIndex::A0 as usize, vcpu_id);
        vcpu.set_gpr(RiscvGprIndex::A1 as usize, arg);
    }

    fn has_hardware_support() -> bool {
        riscv_vcpu::has_hardware_support()
    }

    fn activate_devices(vm: &crate::AxVM) -> AxVmResult {
        vplic_runtime(vm)?.activate()
    }

    fn deactivate_devices(vm: &crate::AxVM) -> AxVmResult {
        vplic_runtime(vm)?.deactivate()
    }

    fn before_vcpu_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) -> AxVmResult {
        sync_vplic_vseip(vm, vcpu)
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
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
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
            } => handle_riscv_mmio_read(
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
            RiscvVmExit::MmioWrite { addr, width, data } => handle_riscv_mmio_write(
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
            RiscvVmExit::SendIPI {
                target_cpu,
                target_cpu_aux,
                send_to_all,
                send_to_self,
                vector,
            } => {
                let targets = <Riscv64Arch as ArchOps>::ipi_targets(
                    vm,
                    vcpu.id(),
                    target_cpu,
                    target_cpu_aux,
                    send_to_all,
                    send_to_self,
                );

                if targets.is_empty() {
                    warn!(
                        "VM[{}] SendIPI has no target: target_cpu={target_cpu:#x}",
                        vm.id()
                    );
                    return Ok(BoundVcpuExit::Complete(VcpuRunAction {
                        waits_for_event: false,
                        stop_reason: None,
                        resets_vm: false,
                        exits_vcpu: false,
                    }));
                }

                super::deliver_riscv_ipi_targets(
                    targets,
                    vcpu.id(),
                    vector as _,
                    |vector| crate::inject_current_vcpu_interrupt(vector),
                    |remote_targets, vector| vm.inject_interrupt_to_vcpu(remote_targets, vector),
                )?;

                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
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
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction> {
        match work {
            RiscvDeferredRunWork::ExternalInterrupt { vector } => {
                Self::after_external_interrupt(vm, vcpu, vector);
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
        Self::deactivate_devices(vm)
    }
}

fn handle_riscv_mmio_read(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    exit: MmioReadExit,
) -> AxVmResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let result = super::handle_mmio_read(vm, vcpu, exit)?;
    sync_vplic_vseip(vm, vcpu)?;
    Ok(result)
}

fn handle_riscv_mmio_write(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    exit: MmioWriteExit,
) -> AxVmResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let result = super::handle_mmio_write::<Riscv64Arch>(vm, exit)?;
    sync_vplic_vseip(vm, vcpu)?;
    Ok(result)
}

fn vplic_runtime(vm: &crate::AxVM) -> AxVmResult<Arc<irq::RiscvPlicRuntime>> {
    vm.get_devices()?
        .services()
        .require::<irq::RiscvPlicRuntimeKey>()
        .map_err(Into::into)
}

fn sync_vplic_vseip(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>) -> AxVmResult {
    let asserted = vplic_runtime(vm)?.vcpu_has_deliverable_irq(vcpu.id())?;
    vcpu.get_arch_vcpu().sync_bound_vseip(asserted)
}

fn handle_riscv_nested_page_fault(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    addr: RiscvGuestPhysAddr,
    access_flags: RiscvAccessFlags,
) -> AxVmResult<BoundVcpuExit<RiscvDeferredRunWork>> {
    let ax_addr = riscv_guest_phys_addr_to_ax(addr);
    if let Some(decoded) = vcpu.get_arch_vcpu().decode_mmio_fault(addr, access_flags) {
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
            RiscvVmExit::MmioWrite { addr, width, data } => {
                super::try_handle_mmio_write::<Riscv64Arch>(
                    vm,
                    MmioWriteExit {
                        addr: riscv_guest_phys_addr_to_ax(addr),
                        width: riscv_access_width_to_ax(width),
                        data,
                    },
                )?
            }
            _ => false,
        };
        if handled {
            sync_vplic_vseip(vm, vcpu)?;
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

    fn sync_bound_vseip(&mut self, asserted: bool) -> AxVmResult {
        riscv_result(self.0.sync_bound_vseip(asserted))
            .map_err(|error| crate::AxVmError::vcpu("synchronize RISC-V VSEIP", error))
    }
}

impl VmArchVcpuOps for AxvmRiscvVcpu {
    type CreateConfig = RiscvVcpuCreateConfig;
    type SetupConfig = ();
    type Exit = RiscvVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        riscv_result(RiscvVCpu::new(vm_id, vcpu_id, config)).map(Self)
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
        riscv_result(self.0.run())
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

pub(crate) struct AxvmRiscvPerCpu(RiscvPerCpu);

impl VmArchPerCpuOps for AxvmRiscvPerCpu {
    fn new(cpu_id: usize) -> BackendResult<Self> {
        riscv_result(RiscvPerCpu::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> BackendResult {
        riscv_result(self.0.hardware_enable())
    }

    fn hardware_disable(&mut self) -> BackendResult {
        riscv_result(self.0.hardware_disable())
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

    fn assert_riscv_exit_type<T: VmArchVcpuOps<Exit = RiscvVmExit>>() {}

    #[test]
    fn axvm_riscv_vcpu_uses_riscv_exit_type() {
        assert_riscv_exit_type::<AxvmRiscvVcpu>();
    }

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
