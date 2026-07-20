use alloc::vec::Vec;
use core::fmt;

use ax_cpu_local::CpuPin;
use ax_kspin::{IrqGuard, PreemptGuard};
use ax_memory_addr::VirtAddr;
use axvm_types::{
    AccessWidth, GuestPhysAddr, MappingFlags, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError as BackendError, VmBackendResult as BackendResult,
};
use riscv_h::register::hvip;
use riscv_vcpu::{
    GprIndex as RiscvGprIndex, RiscvAccessFlags, RiscvAccessWidth, RiscvBoundExit,
    RiscvGuestPhysAddr, RiscvHostOps, RiscvHostPhysAddr, RiscvHostVirtAddr,
    RiscvNestedPagingConfig, RiscvPerCpu, RiscvVCpu, RiscvVcpuCreateConfig, RiscvVcpuError,
    RiscvVcpuResult, RiscvVmExit,
};

use super::{
    ArchOps, BoundVcpuExit, CommonDeferredRunWork, HypercallExit, MmioReadExit, MmioWriteExit,
    VcpuRunAction,
};
use crate::{
    AxVmResult, StopReason,
    architecture::ops::default_vcpu_affinities,
    host::{HostMemory, default_host},
    irq::RiscvPlatformIrq,
};

mod capabilities;
mod completion_restore;
#[path = "../../architecture/cpu_up.rs"]
mod cpu_up;
pub(crate) mod fdt;
mod forwarded_ingress;
mod images;
mod irq;
mod npt;
mod owner_doorbell;
mod route_transaction;
mod vm;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
use cpu_up::{CpuUpExit, CpuUpOps};
pub use images::ImageLoader;
use irq::forward_unbound_physical_irq;

pub(crate) struct Riscv64Arch;

#[derive(Clone, Copy, Debug)]
pub(crate) enum RiscvDeferredRunWork {
    Common(CommonDeferredRunWork),
    CpuUp(CpuUpExit),
    NestedPageFault {
        addr: RiscvGuestPhysAddr,
        access_flags: RiscvAccessFlags,
    },
    WaitForEvent,
}

impl From<CommonDeferredRunWork> for RiscvDeferredRunWork {
    fn from(work: CommonDeferredRunWork) -> Self {
        Self::Common(work)
    }
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

    fn has_hardware_support() -> bool {
        let preempt_guard = PreemptGuard::new();
        riscv_vcpu::has_hardware_support(preempt_guard.cpu_pin())
    }

    fn register_platform_irq_injector() {
        // SAFETY: `forward_unbound_physical_irq` only publishes a canonical
        // source/generation into preallocated atomic ingress and directly
        // wakes the fixed owner. Its code and referenced route live until
        // shutdown and it cannot unwind.
        let registered = unsafe { RiscvPlatformIrq::register_sink(forward_unbound_physical_irq) };
        assert!(
            registered,
            "RISC-V platform IRQ sink is already owned by another monitor"
        );
    }

    fn revoke_guest_irq_routes(vm: &crate::AxVMRef) -> AxVmResult {
        irq::revoke_guest_irq_routes(vm.id())
    }

    fn prepare_vcpu_irq_owner(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult<Option<crate::architecture::ops::VcpuIrqOwnerSession>> {
        irq::prepare_guest_irq_owner_session(vm, vcpu)
    }

    fn before_first_run(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        let wake_installed = match ax_std::os::arceos::task::current_thread_handle() {
            Ok(thread) => {
                let wake = thread.wake_handle();
                vcpu.with_arch_vcpu("install RISC-V vPLIC wake target", |arch_vcpu| {
                    arch_vcpu.install_vplic_wake_target(wake)
                })?
            }
            Err(error) => {
                return Err(crate::AxVmError::resource_unavailable(
                    "RISC-V vPLIC owner wake handle",
                    error,
                ));
            }
        };

        // Physical IRQ affinity is VM-wide. vCPU0 is the deterministic owner;
        // allowing every first-run hook to write it would make the last vCPU
        // scheduled decide the route for the whole VM.
        if !irq::guest_irq_owner_session_required(vm, vcpu.id()) {
            return Ok(());
        }
        if !wake_installed {
            return Err(crate::AxVmError::resource_unavailable(
                "RISC-V passthrough vPLIC",
                "the route owner has no stable scheduler wake target",
            ));
        }
        let Some(cpu_id) = vcpu.phys_cpu_set().and_then(single_cpu_in_mask) else {
            return Err(crate::AxVmError::invalid_config(format_args!(
                "RISC-V passthrough VM[{}] VCpu[{}] requires exactly one fixed host CPU",
                vm.id(),
                vcpu.id()
            )));
        };
        let irq_sources = vm.with_config(|config| config.pass_through_irqs().to_vec());
        let route = vcpu
            .with_arch_vcpu("acquire RISC-V platform IRQ route", |arch_vcpu| {
                arch_vcpu.vplic_platform_binding()
            })?
            .ok_or_else(|| {
                crate::AxVmError::resource_unavailable(
                    "RISC-V passthrough vPLIC",
                    "the owner vCPU has no vPLIC binding",
                )
            })?;
        // Route ownership is physical-CPU state. One pin must cover the live
        // identity check, controller preparation, publication, and endpoint
        // activation so IRQ-return preemption cannot move the transaction to a
        // different CPU between validation and commit.
        let route_guard = PreemptGuard::new();
        let current_cpu = current_cpu_index(route_guard.cpu_pin())?;
        if current_cpu != cpu_id {
            return Err(crate::AxVmError::resource_conflict(
                "RISC-V passthrough CPU pin",
                format_args!(
                    "VM[{}] VCpu[{}] is running on host CPU {current_cpu}, but physical PLIC \
                     ownership requires configured CPU {cpu_id}",
                    vm.id(),
                    vcpu.id()
                ),
            ));
        }
        let activated =
            route.install_platform_route(cpu_id, &irq_sources, route_guard.cpu_pin())?;
        if !activated.is_activated() {
            return Err(crate::AxVmError::interrupt(
                "prepare RISC-V PLIC passthrough route",
                format_args!(
                    "status={:?}, source={}, target_cpu={cpu_id}",
                    activated.status, activated.source
                ),
            ));
        }
        Ok(())
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

    fn after_mmio_read(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        publish_vplic_guest_state(vcpu)
    }

    fn after_mmio_write(
        _vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        publish_vplic_guest_state(vcpu)
    }

    fn handle_vcpu_exit_bound<'cpu>(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit<'cpu>,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>
    where
        Self::VCpu: 'cpu,
    {
        let exit_event = exit.event();
        if let RiscvVmExit::ExternalInterrupt { vector } = exit_event {
            debug!("VM[{}] run VCpu[{}] get irq {vector}", vm.id(), vcpu.id());
            capture_bound_external_interrupt(vm, vcpu, exit.vplic_binding(), vector as usize)?;
            drop(exit);
            return Ok(BoundVcpuExit::Continue);
        }

        // Non-external exits no longer need host IRQ ownership. Release the
        // entry token before helpers access the backend or perform OS work.
        drop(exit);
        match exit_event {
            RiscvVmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
            RiscvVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => super::handle_mmio_read::<Self>(
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
                vcpu,
                MmioWriteExit {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    width: riscv_access_width_to_ax(width),
                    data,
                },
            ),
            RiscvVmExit::NestedPageFault { addr, access_flags } => Ok(BoundVcpuExit::Defer(
                RiscvDeferredRunWork::NestedPageFault { addr, access_flags },
            )),
            RiscvVmExit::ExternalInterrupt { .. } => unreachable!(),
            RiscvVmExit::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => Ok(BoundVcpuExit::Defer(RiscvDeferredRunWork::CpuUp(
                CpuUpExit {
                    target_cpu,
                    entry_point: riscv_guest_phys_addr_to_ax(entry_point),
                    arg,
                },
            ))),
            RiscvVmExit::CpuDown { state } => {
                warn!(
                    "VM[{}] run VCpu[{}] CpuDown state {state:#x}",
                    vm.id(),
                    vcpu.id()
                );
                Ok(BoundVcpuExit::Defer(RiscvDeferredRunWork::WaitForEvent))
            }
            RiscvVmExit::Halt => {
                debug!("VM[{}] run VCpu[{}] Halt", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Defer(RiscvDeferredRunWork::WaitForEvent))
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
    ) -> AxVmResult<VcpuRunAction> {
        match work {
            RiscvDeferredRunWork::Common(work) => super::finish_deferred::<Self>(vm, vcpu, work),
            RiscvDeferredRunWork::CpuUp(exit) => cpu_up::finish::<Self>(vm, vcpu, exit),
            RiscvDeferredRunWork::NestedPageFault { addr, access_flags } => {
                handle_riscv_nested_page_fault(vm, vcpu, addr, access_flags)
            }
            RiscvDeferredRunWork::WaitForEvent => Ok(VcpuRunAction {
                waits_for_event: true,
                stop_reason: None,
            }),
        }
    }
}

fn publish_vplic_guest_state(vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>) -> AxVmResult {
    let published = vcpu.with_arch_vcpu("publish RISC-V vPLIC guest state", |arch_vcpu| {
        arch_vcpu.publish_vplic_guest_state_changes()
    })?;
    published.map_err(|()| {
        crate::AxVmError::interrupt(
            "publish RISC-V vPLIC guest state",
            "the fixed platform owner could not be notified",
        )
    })
}

fn capture_bound_external_interrupt(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    vplic: Option<&irq::VplicVcpuBinding>,
    vector: usize,
) -> AxVmResult {
    let Some(claim) = RiscvPlatformIrq::claim_and_mask(vector) else {
        return Ok(());
    };

    let forwarded = vplic.is_some_and(|vplic| vplic.forward_physical_irq(claim));
    if !forwarded {
        return Err(crate::AxVmError::interrupt(
            "publish captured RISC-V PLIC source",
            format_args!(
                "VM[{}] VCpu[{}] cannot transfer masked source {} to the fixed vPLIC owner",
                vm.id(),
                vcpu.id(),
                claim.source()
            ),
        ));
    }
    Ok(())
}

fn handle_riscv_nested_page_fault(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    addr: RiscvGuestPhysAddr,
    access_flags: RiscvAccessFlags,
) -> AxVmResult<VcpuRunAction> {
    let ax_addr = riscv_guest_phys_addr_to_ax(addr);
    if vm.get_devices()?.find_mmio_dev(ax_addr).is_some() {
        let Some(decoded) = vcpu.with_arch_vcpu("decode RISC-V MMIO fault", |arch_vcpu| {
            arch_vcpu.decode_mmio_fault(addr, access_flags)
        })?
        else {
            warn!(
                "VM[{}] VCpu[{}] nested page fault at {:#x} maps MMIO but cannot be decoded",
                vm.id(),
                vcpu.id(),
                ax_addr.as_usize()
            );
            return Ok(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
            });
        };
        return finish_decoded_riscv_mmio(vm, vcpu, decoded);
    }

    let ax_flags = riscv_access_flags_to_ax(access_flags);
    if vm.handle_nested_page_fault(ax_addr, ax_flags) {
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
    } else {
        warn!(
            "VM[{}] VCpu[{}] unhandled nested page fault at {:#x}, access={:?}",
            vm.id(),
            vcpu.id(),
            ax_addr.as_usize(),
            ax_flags
        );
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
    }
}

fn finish_decoded_riscv_mmio(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmRiscvVcpu>,
    exit: RiscvVmExit,
) -> AxVmResult<VcpuRunAction> {
    let work = match exit {
        RiscvVmExit::MmioRead {
            addr,
            width,
            reg,
            reg_width,
            signed_ext,
        } => CommonDeferredRunWork::MmioRead(MmioReadExit {
            addr: riscv_guest_phys_addr_to_ax(addr),
            width: riscv_access_width_to_ax(width),
            reg,
            reg_width: riscv_access_width_to_ax(reg_width),
            signed_ext,
        }),
        RiscvVmExit::MmioWrite { addr, width, data } => {
            CommonDeferredRunWork::MmioWrite(MmioWriteExit {
                addr: riscv_guest_phys_addr_to_ax(addr),
                width: riscv_access_width_to_ax(width),
                data,
            })
        }
        _ => unreachable!("RISC-V MMIO decode returned a non-MMIO exit"),
    };
    super::finish_deferred::<Riscv64Arch>(vm, vcpu, work)
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

pub(crate) struct AxvmRiscvVcpuCreateConfig {
    backend: RiscvVcpuCreateConfig,
    vplic: Option<irq::VplicVcpuBinding>,
}

impl AxvmRiscvVcpuCreateConfig {
    pub(crate) const fn new(
        backend: RiscvVcpuCreateConfig,
        vplic: Option<irq::VplicVcpuBinding>,
    ) -> Self {
        Self { backend, vplic }
    }
}

pub(crate) struct AxvmRiscvVcpu {
    backend: RiscvVCpu<AxvmRiscvHostOps>,
    vplic: Option<irq::VplicVcpuBinding>,
}

pub(crate) struct AxvmRiscvBoundExit<'cpu> {
    backend: Option<RiscvBoundExit<'cpu>>,
    vplic: Option<irq::VplicVcpuBinding>,
    irq_guard: Option<IrqGuard>,
}

impl AxvmRiscvBoundExit<'_> {
    const fn event(&self) -> RiscvVmExit {
        match &self.backend {
            Some(backend) => backend.event(),
            None => unreachable!(),
        }
    }

    const fn vplic_binding(&self) -> Option<&irq::VplicVcpuBinding> {
        self.vplic.as_ref()
    }
}

impl fmt::Debug for AxvmRiscvBoundExit<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.event().fmt(formatter)
    }
}

impl Drop for AxvmRiscvBoundExit<'_> {
    fn drop(&mut self) {
        // Restore the backend's nested SIE snapshot while the outer runtime
        // guard still keeps global IRQ delivery disabled. The outer guard is
        // the only owner allowed to restore the caller's original IRQ state.
        drop(self.backend.take());
        drop(self.irq_guard.take());
    }
}

impl AxvmRiscvVcpu {
    fn install_vplic_wake_target(
        &mut self,
        wake: ax_std::os::arceos::task::ThreadWakeHandle,
    ) -> bool {
        if let Some(vplic) = &self.vplic {
            vplic.install_wake_target(wake);
            true
        } else {
            false
        }
    }

    fn vplic_platform_binding(&self) -> Option<irq::VplicVcpuBinding> {
        self.vplic.clone()
    }

    fn publish_vplic_guest_state_changes(&self) -> Result<(), ()> {
        self.vplic
            .as_ref()
            .map_or(Ok(()), irq::VplicVcpuBinding::publish_guest_state_changes)
    }

    fn decode_mmio_fault(
        &mut self,
        addr: RiscvGuestPhysAddr,
        access_flags: RiscvAccessFlags,
    ) -> Option<RiscvVmExit> {
        self.backend.decode_mmio_fault(addr, access_flags)
    }

    fn unmask_completed_physical_irqs(
        &self,
        completions: &irq::ForwardedCompletionBatch,
        cpu_pin: &CpuPin,
    ) -> Result<(), usize> {
        let bound_cpu_pin = ax_percpu::bound_current(cpu_pin).map_err(|_| 0usize)?;
        let current_cpu = bound_cpu_pin.cpu_index().as_usize();
        for (index, claim) in completions.claims().iter().enumerate() {
            let Some(claim) = claim else {
                return Err(index);
            };
            if !RiscvPlatformIrq::unmask(*claim, current_cpu) {
                return Err(index);
            }
        }
        Ok(())
    }

    fn sync_vplic_line(&mut self) -> BackendResult {
        let Some(vplic) = &self.vplic else {
            return Ok(());
        };
        let asserted = vplic
            .take_line_level()
            .map_err(|_| BackendError::InvalidState)?;
        // SAFETY: VmArchVcpuOps::run receives the CpuPin held by AxVM's Bound
        // owner. The live HVIP belongs to this vCPU until the matching unbind.
        unsafe {
            if asserted {
                hvip::set_vseip();
            } else {
                hvip::clear_vseip();
            }
        }
        Ok(())
    }
}

impl VmArchVcpuOps for AxvmRiscvVcpu {
    type CreateConfig = AxvmRiscvVcpuCreateConfig;
    type SetupConfig = ();
    type Exit<'cpu> = AxvmRiscvBoundExit<'cpu>;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        let backend = riscv_result(RiscvVCpu::new(vm_id, vcpu_id, config.backend))?;
        Ok(Self {
            backend,
            vplic: config.vplic,
        })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        riscv_result(self.backend.set_entry(ax_guest_phys_addr_to_riscv(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        riscv_result(
            self.backend
                .set_nested_page_table(ax_nested_paging_to_riscv(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        riscv_result(self.backend.setup(config))
    }

    fn run<'cpu>(&'cpu mut self, cpu_pin: &'cpu CpuPin) -> BackendResult<Self::Exit<'cpu>> {
        let completion_batch = if let Some(vplic) = &self.vplic
            && vplic.is_platform_owner()
        {
            vplic
                .drain_forwarded_ingress()
                .map_err(|()| BackendError::InvalidState)?;
            Some(
                vplic
                    .take_completed_claim_batch()
                    .map_err(|()| BackendError::InvalidState)?,
            )
        } else {
            None
        };
        let irq_guard = IrqGuard::new();
        if let Some(completions) = &completion_batch
            && let Err(first_uncompleted) =
                self.unmask_completed_physical_irqs(completions, cpu_pin)
        {
            drop(irq_guard);
            let restored = self.vplic.as_ref().is_some_and(|vplic| {
                vplic.restore_completed_claim_batch(completions, first_uncompleted)
            });
            if !restored {
                return Err(BackendError::InvalidState);
            }
            return Err(BackendError::InvalidState);
        }
        if let Some(completions) = &completion_batch
            && !completions.claims().is_empty()
            && self
                .vplic
                .as_ref()
                .is_none_or(|vplic| vplic.finish_completed_claim_batch(completions).is_err())
        {
            return Err(BackendError::InvalidState);
        }
        self.sync_vplic_line()?;
        let vplic = self.vplic.clone();
        let backend = riscv_result(self.backend.run(cpu_pin))?;
        Ok(AxvmRiscvBoundExit {
            backend: Some(backend),
            vplic,
            irq_guard: Some(irq_guard),
        })
    }

    fn bind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        riscv_result(self.backend.bind(cpu_pin))
    }

    fn unbind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        riscv_result(self.backend.unbind(cpu_pin))
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.backend.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        // This only updates vCPU-owned saved state. The next Bound run merges
        // the vPLIC's latest context line into the live physical HVIP.
        riscv_result(self.backend.inject_interrupt(vector))
    }

    fn set_return_value(&mut self, val: usize) {
        self.backend.set_return_value(val);
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

    fn hardware_enable(&mut self, cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        riscv_result(self.0.hardware_enable(cpu_pin))
    }

    fn hardware_disable(&mut self, cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        riscv_result(self.0.hardware_disable(cpu_pin))
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

fn single_cpu_in_mask(mask: usize) -> Option<usize> {
    (mask.count_ones() == 1).then_some(mask.trailing_zeros() as usize)
}

fn current_cpu_index(cpu_pin: &CpuPin) -> AxVmResult<usize> {
    let bound_cpu_pin = ax_percpu::bound_current(cpu_pin).map_err(|error| {
        crate::AxVmError::resource_unavailable(
            "RISC-V passthrough CPU pin",
            format_args!("the current CPU area is not bound: {error}"),
        )
    })?;
    Ok(ax_percpu::current_cpu_index(&bound_cpu_pin)
        .expect("BoundCpuPin must carry a logical CPU index")
        .as_usize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_riscv_exit_type<T>()
    where
        for<'cpu> T: VmArchVcpuOps<Exit<'cpu> = AxvmRiscvBoundExit<'cpu>>,
    {
    }

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
