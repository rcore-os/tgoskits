//! AxVM RISC-V adapter above `ax_cpu::virtualization`.
//!
//! The architecture owns one plain vCPU backend value per vCPU task, the
//! prepared entry payload, the owned exit record, and the register completion.
//! All hardware entry, CPU binding, interrupt masking, and exit interpretation
//! live in the shared engine; this module only supplies architecture primitives
//! and interprets already-owned exits without touching a shared backend.

use std::sync::Arc;

use ax_memory_addr::VirtAddr;
use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult, *};
use rustsbi::SbiRet;

use self::policy::{GprIndex as RiscvGprIndex, *};
use super::*;
use crate::{
    AxVM, AxVmResult, StopReason,
    architecture::ops::CpuOn,
    engine::{VcpuAction, WaitReason},
    host::*,
    irq::model::{DeliveryToken, PendingVcpuInterrupt, VcpuLocalInterrupts, VcpuLocalTimer},
    runtime::{
        QueuedVcpuInterrupt,
        hvc::{GuestRequest, HyperCallAbi},
    },
    services::{RunServices, RunSignals, VcpuWait},
    vm::AxVMResources,
};

mod capabilities;
mod console;
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

/// Per-runtime entry payload for the RISC-V architecture.
///
/// It retains only the fixed guest-hart topology and a narrow VM-local vPLIC
/// delivery port extracted from the owning resources. The complete VM,
/// `RunServices`, device runtime, physical IRQ bridge, and sleepable state locks
/// are never reachable from a hardware entry.
pub(crate) struct RiscvEntry {
    plic: irq::VplicDeliveryPort,
    topology: hsm::HartTopology,
}

impl RiscvEntry {
    /// Attaches architecture-local routing to one interpreted exit.
    ///
    /// Topology-dependent fields and the MMIO/device classification are
    /// resolved here, while the hardware backend is still owned by this task,
    /// so the unbound exit handler never queries the complete VM.
    fn resolve_exit(&self, exit: RiscvVmExit) -> RiscvExit {
        match exit {
            RiscvVmExit::Hypercall { nr, args } => RiscvExit::Hypercall { nr, args },
            RiscvVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
                advance,
            } => RiscvExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
                advance,
            },
            RiscvVmExit::MmioWrite {
                addr,
                width,
                data,
                advance,
            } => RiscvExit::MmioWrite {
                addr,
                width,
                data,
                advance,
                touches_vplic: self
                    .plic
                    .contains_guest_addr(riscv_guest_phys_addr_to_ax(addr)),
            },
            RiscvVmExit::NestedPageFault { addr, access_flags } => {
                RiscvExit::NestedPageFault { addr, access_flags }
            }
            RiscvVmExit::SendIpi(request) => RiscvExit::SendIpi {
                request,
                targets: None,
            },
            RiscvVmExit::CpuUp {
                target_cpu,
                entry_point,
                arg,
            } => RiscvExit::CpuOn {
                target_vcpu_id: usize::try_from(target_cpu)
                    .ok()
                    .and_then(|guest_hart_id| self.topology.resolve_hart(guest_hart_id)),
                entry_point,
                context_id: arg as usize,
            },
            RiscvVmExit::SbiCall(call) => RiscvExit::SbiCall(call),
            RiscvVmExit::CpuDown => RiscvExit::CpuOff,
            RiscvVmExit::SbiStandby => RiscvExit::SbiStandby,
            RiscvVmExit::SystemDown => RiscvExit::SystemDown,
            RiscvVmExit::Nothing => RiscvExit::Nothing,
        }
    }
}

impl ArchOps for Riscv64Arch {
    type VCpu = AxvmRiscvVcpu;
    type PerCpu = AxvmRiscvPerCpu;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;
    type Entry = RiscvEntry;
    type Exit = RiscvExit;
    type Completion = RiscvCompletion;

    fn has_hardware_support() -> bool {
        ax_cpu::capability::has_hypervisor_extension()
    }

    fn invalidate_translations(
        _entry: &Self::Entry,
        _old_root: axvm_types::NestedPagingConfig,
    ) -> AxVmResult {
        // The control owner runs this on every pCPU that could cache a
        // translation for the retired root, in a synchronous rendezvous after
        // all vCPUs are unloaded, and retains the root's backing memory until
        // every participant completes the invalidation.
        //
        // AxVM leaves the guest VMID at zero, so a root-scoped fence cannot be
        // expressed; `hfence.gvma` retires every cached G-stage translation for
        // the current hart before that pCPU can run any guest again.
        //
        // SAFETY: the caller holds HS mode with the H extension enabled, keeps
        // the hart pinned for this call, and serializes table mutation as
        // described above.
        unsafe { ax_cpu::virtualization::invalidate_gstage_translations() };
        Ok(())
    }

    fn prepare_entry(
        resources: &AxVMResources,
        _signals: Arc<RunSignals>,
    ) -> AxVmResult<Self::Entry> {
        let plic = resources
            .devices()?
            .services()
            .require::<irq::RiscvPlicRuntimeKey>()
            .map_err(|error| {
                crate::AxVmError::device("resolve RISC-V interrupt controller", error)
            })?
            .delivery_port();
        let topology =
            hsm::HartTopology::new(&resources.phys_cpu_ls.get_vcpu_affinities_pcpu_ids());
        Ok(RiscvEntry { plic, topology })
    }

    fn enter_runtime(vm: &mut AxVM, signals: &Arc<RunSignals>) -> AxVmResult {
        vplic_runtime(vm)?.activate(signals.clone())
    }

    fn exit_runtime(vm: &mut AxVM, _signals: &Arc<RunSignals>) -> AxVmResult {
        vplic_runtime(vm)?.deactivate()
    }

    fn prepare_vcpu(_vcpu: &mut Self::VCpu, _entry: &Self::Entry) -> AxVmResult {
        Ok(())
    }

    /// Rederives VSEIP from the VM-local vPLIC on the loaded owner.
    ///
    /// The vPLIC remains the sole owner of pending and delivery state, and no
    /// remote register write ever reaches a different vCPU. This hook runs
    /// inside the engine's loaded scope, so the controller-derived level is
    /// committed to this vCPU's saved CSR image and reflected into hardware.
    fn before_guest(vcpu: &mut Self::VCpu, vcpu_id: usize, entry: &Self::Entry) -> AxVmResult {
        VcpuLocalInterrupts::prepare_entry(vcpu).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error(
                "prepare RISC-V local interrupt state",
                riscv_error_to_backend(error),
            )
        })?;
        let asserted = entry.plic.vcpu_has_deliverable_irq(vcpu_id)?;
        vcpu.sync_vseip_level(asserted)
            .map_err(|error| crate::vcpu::map_vcpu_backend_error("synchronize RISC-V VSEIP", error))
    }

    fn complete(
        vcpu: &mut Self::VCpu,
        _entry: &Self::Entry,
        completion: Self::Completion,
    ) -> AxVmResult {
        match completion {
            RiscvCompletion::None => {}
            RiscvCompletion::Gpr { register, value } => vcpu.set_gpr(register, value),
            RiscvCompletion::Retire {
                register,
                value,
                advance,
            } => {
                if let Some(register) = register {
                    vcpu.set_gpr(register, value);
                }
                vcpu.advance_pc(advance);
            }
            RiscvCompletion::SbiRet { error, value } => {
                vcpu.set_gpr(RiscvGprIndex::A0 as usize, error);
                vcpu.set_gpr(RiscvGprIndex::A1 as usize, value);
            }
            RiscvCompletion::Ipi {
                request,
                completion,
            } => {
                vcpu.complete_ipi(request, completion);
            }
        }
        Ok(())
    }

    /// Interprets one durable hardware exit while the backend is still loaded.
    ///
    /// Machine `process_exit` and guest-fault instruction decoding run here, so
    /// the unbound handler receives an owned record and never re-enters the
    /// hardware backend or decodes a guest instruction after unloading.
    fn capture_exit(
        vcpu: &mut Self::VCpu,
        entry: &Self::Entry,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<Self::Exit> {
        let vm_exit = vcpu.process_exit(exit).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error(
                "interpret RISC-V exit",
                riscv_error_to_backend(error),
            )
        })?;
        Ok(entry.resolve_exit(vm_exit))
    }

    fn finish_exit(
        vcpu: &mut Self::VCpu,
        entry: &Self::Entry,
        mut exit: Self::Exit,
    ) -> AxVmResult<Self::Exit> {
        // Resolving a hart set allocates its owned target list. Hardware is
        // unloaded and CPU/IRQ context restored before this task-side stage.
        if let RiscvExit::SendIpi { request, targets } = &mut exit {
            *targets = ipi::resolve_targets(
                request.hart_mask(),
                request.hart_mask_base(),
                || entry.topology.vcpu_ids(),
                |hart| entry.topology.resolve_hart(hart),
            )
            .ok()
            .map(Vec::into_boxed_slice);
        }
        if let RiscvExit::SbiCall(call) = exit
            && !console::is_console_call(call)
        {
            let result = vcpu.forward_task_sbi(call).map_err(|error| {
                crate::vcpu::map_vcpu_backend_error(
                    "forward RISC-V SBI call",
                    riscv_error_to_backend(error),
                )
            })?;
            exit = RiscvExit::SbiResult {
                error: result.error,
                value: result.value,
            };
        }
        Ok(exit)
    }

    fn handle_exit(
        exit: Self::Exit,
        vcpu_id: usize,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<Self::Completion, GuestRequest>> {
        match exit {
            RiscvExit::Hypercall { nr, args } => handle_hypercall::<Self>(
                services,
                vcpu_id,
                HypercallExit { nr, args },
                HyperCallAbi::native(),
            ),
            RiscvExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
                advance,
            } => {
                let access = MmioReadExit {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    width: riscv_access_width_to_ax(width),
                    reg,
                    reg_width: riscv_access_width_to_ax(reg_width),
                    signed_ext,
                };
                let Some(completion) = try_handle_mmio_read(services, vcpu_id, access)? else {
                    // The G-stage fault decoded as a load but no device owns
                    // the address, so it is guest memory that must be mapped
                    // lazily. Hand the original fault back to the control
                    // owner; its reply re-enters with the default completion,
                    // leaving the instruction pointer on the faulting load.
                    return Ok(VcpuAction::Control(GuestRequest::NestedFault {
                        addr: riscv_guest_phys_addr_to_ax(addr),
                        access_flags: MappingFlags::READ,
                    }));
                };
                // Retire the emulated load only now that the device produced
                // its value: the register write and the instruction-pointer
                // step are both committed by this durable completion.
                Ok(VcpuAction::Reenter(
                    RiscvCompletion::from(completion).retire(advance),
                ))
            }
            RiscvExit::MmioWrite {
                addr,
                width,
                data,
                advance,
                touches_vplic,
            } => {
                let access = MmioWriteExit {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    width: riscv_access_width_to_ax(width),
                    data,
                };
                if !try_handle_mmio_write(services, vcpu_id, access)? {
                    // No device accepted the decoded store; report the
                    // original nested page fault with the instruction pointer
                    // still parked on the store.
                    return Ok(VcpuAction::Control(GuestRequest::NestedFault {
                        addr: riscv_guest_phys_addr_to_ax(addr),
                        access_flags: MappingFlags::WRITE,
                    }));
                }
                if touches_vplic {
                    wake_vplic_targets(services)?;
                }
                // Retire the completed store by stepping over it; it has no
                // destination register.
                Ok(VcpuAction::Reenter(
                    RiscvCompletion::default().retire(advance),
                ))
            }
            RiscvExit::NestedPageFault { addr, access_flags } => {
                Ok(VcpuAction::Control(GuestRequest::NestedFault {
                    addr: riscv_guest_phys_addr_to_ax(addr),
                    access_flags: riscv_access_flags_to_ax(access_flags),
                }))
            }
            RiscvExit::SendIpi { request, targets } => {
                let completion = match targets {
                    None => RiscvIpiCompletion::InvalidParameter,
                    Some(targets) => match ipi::deliver(&targets, |target, interrupt| {
                        services.signals().publish(target, interrupt)?;
                        services.signals().kick(target)
                    }) {
                        Ok(()) => RiscvIpiCompletion::Success,
                        Err(error) => {
                            warn!(
                                "VCpu[{vcpu_id}] SBI IPI delivery to VCpu[{}] failed: {:?}",
                                error.target_vcpu_id, error.source
                            );
                            RiscvIpiCompletion::Failed
                        }
                    },
                };
                Ok(VcpuAction::Reenter(RiscvCompletion::Ipi {
                    request,
                    completion,
                }))
            }
            RiscvExit::CpuOn {
                target_vcpu_id,
                entry_point,
                context_id,
            } => match target_vcpu_id {
                Some(target_vcpu_id) => Ok(VcpuAction::Control(GuestRequest::CpuOn {
                    target_vcpu_id,
                    entry_point: riscv_guest_phys_addr_to_ax(entry_point),
                    context_id,
                    abi: HyperCallAbi::native(),
                })),
                None => Ok(VcpuAction::Reenter(RiscvCompletion::SbiRet {
                    error: SbiRet::invalid_param().error,
                    value: 0,
                })),
            },
            RiscvExit::CpuOff => Ok(VcpuAction::Control(GuestRequest::CpuOff {
                abi: HyperCallAbi::native(),
            })),
            RiscvExit::SbiCall(call) => Ok(VcpuAction::Reenter(console::handle(call, services))),
            RiscvExit::SbiResult { error, value } => {
                Ok(VcpuAction::Reenter(RiscvCompletion::SbiRet {
                    error,
                    value,
                }))
            }
            RiscvExit::SbiStandby => Ok(VcpuAction::Wait(WaitReason {
                return_value: Some(0),
            })),
            RiscvExit::SystemDown => Ok(VcpuAction::Stop(StopReason::SystemDown)),
            RiscvExit::Nothing => Ok(VcpuAction::Reenter(RiscvCompletion::None)),
        }
    }

    /// RISC-V consumes its supervisor-software source from `RunSignals::publish`
    /// rather than an architecture-neutral vector, so the injected cause carries
    /// the complete `scause` interrupt bit.
    fn inject_vcpu_interrupt(vcpu: &mut Self::VCpu, interrupt: PendingVcpuInterrupt) -> AxVmResult {
        VcpuLocalInterrupts::inject(vcpu, interrupt).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error(
                "inject RISC-V vCPU interrupt",
                riscv_error_to_backend(error),
            )
        })
    }

    fn suspend_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        VcpuLocalTimer::suspend(vcpu).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error(
                "suspend RISC-V local timer",
                riscv_error_to_backend(error),
            )
        })
    }

    fn resume_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        VcpuLocalTimer::resume(vcpu).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error(
                "resume RISC-V local timer",
                riscv_error_to_backend(error),
            )
        })
    }

    fn quiet_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        VcpuLocalTimer::cancel(vcpu).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error(
                "cancel RISC-V local timer",
                riscv_error_to_backend(error),
            )
        })
    }

    fn inject_arch_interrupt(
        _vcpu: &mut Self::VCpu,
        _vcpu_id: usize,
        _entry: &Self::Entry,
        _interrupt: QueuedVcpuInterrupt,
    ) -> AxVmResult {
        // Every RISC-V queued interrupt is consumed as a virtual pending source;
        // the shared engine never reaches this path on RISC-V.
        Ok(())
    }

    fn wait_for_event(
        _vcpu: &mut Self::VCpu,
        vcpu_id: usize,
        entry: &Self::Entry,
        wait: &VcpuWait,
    ) -> AxVmResult {
        let plic = entry.plic.clone();
        // The canonical predicate owns the wake, park, IRQ, and work atomics;
        // only the controller-derived VSEIP state is added here.
        wait.wait_until(move || plic.vcpu_has_deliverable_irq(vcpu_id).unwrap_or(false));
        Ok(())
    }
}

impl CpuOn for Riscv64Arch {
    fn initialize_cpu_on(
        vcpu: &mut Self::VCpu,
        entry: axvm_types::GuestPhysAddr,
        argument: usize,
    ) -> AxVmResult {
        vcpu.initialize_cpu_on(entry, argument).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error("initialize RISC-V CPU_ON target", error)
        })
    }
}

/// Wakes every run-bound vCPU after a guest vPLIC register access.
///
/// An enable, priority, threshold, or claim/complete write can make *other*
/// vCPUs deliverable, so the exiting issuer alone is not enough. The vPLIC is
/// still the sole owner of pending and delivery state: it published the new
/// state before this call, and each woken owner rederives VSEIP from the
/// controller at its next bound `before_guest`. Wakes happen here in task
/// context, outside every controller raw guard, and are derived from the
/// run's fixed active-vCPU mask rather than the complete VM. A vCPU with no
/// live activation is simply not deliverable yet and is skipped.
fn wake_vplic_targets(services: &RunServices) -> AxVmResult {
    let signals = services.signals();
    let mut active = services.active_mask();
    while active != 0 {
        let vcpu_id = active.trailing_zeros() as usize;
        active &= active - 1;
        if let Err(error) = signals.kick(vcpu_id) {
            trace!("RISC-V vPLIC change could not wake vCPU {vcpu_id}: {error:?}");
        }
    }
    Ok(())
}

fn vplic_runtime(vm: &AxVM) -> AxVmResult<Arc<irq::RiscvPlicRuntime>> {
    vm.get_devices()?
        .services()
        .require::<irq::RiscvPlicRuntimeKey>()
        .map_err(Into::into)
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

pub(crate) struct AxvmRiscvVcpu {
    backend: RiscvVcpu<AxvmRiscvHostOps>,
    vcpu_id: usize,
}

impl AxvmRiscvVcpu {
    /// Interprets one captured hardware exit into a RISC-V policy exit.
    fn process_exit(&mut self, exit: ax_cpu::virtualization::Exit) -> RiscvVcpuResult<RiscvVmExit> {
        self.backend.process_exit(exit)
    }

    /// Initializes a hart started through the SBI HSM extension.
    fn initialize_cpu_on(&mut self, entry: GuestPhysAddr, context_id: usize) -> BackendResult {
        riscv_result(
            self.backend
                .initialize_cpu_on(ax_guest_phys_addr_to_riscv(entry), context_id),
        )
    }

    /// Synchronizes controller-derived VSEIP state on the loaded owner.
    fn sync_vseip_level(&mut self, asserted: bool) -> BackendResult {
        riscv_result(self.backend.sync_vseip_level(asserted))
    }

    /// Completes a previously returned SBI IPI request with its original ABI.
    fn complete_ipi(&mut self, request: RiscvIpiRequest, completion: RiscvIpiCompletion) {
        self.backend.complete_ipi(request, completion);
    }

    fn forward_task_sbi(&mut self, call: RiscvSbiCall) -> RiscvVcpuResult<SbiRet> {
        self.backend.forward_task_sbi(call)
    }

    /// Steps the saved guest instruction pointer by one retired emulation.
    ///
    /// Touches only the plain register image, so it is valid while the backend
    /// is unloaded: the engine commits completions after hardware retirement.
    fn advance_pc(&mut self, instr_len: usize) {
        self.backend.advance_pc(instr_len);
    }
}

impl VcpuLocalInterrupts for AxvmRiscvVcpu {
    type Snapshot = usize;
    type Completion = ();
    type Error = RiscvVcpuError;

    fn prepare_entry(&mut self) -> Result<Self::Snapshot, Self::Error> {
        Ok(self.backend.pending_interrupt_snapshot())
    }

    fn inject(&mut self, interrupt: PendingVcpuInterrupt) -> Result<(), Self::Error> {
        const SCAUSE_INTERRUPT_BIT: usize = 1 << (usize::BITS - 1);
        self.backend
            .inject_interrupt(SCAUSE_INTERRUPT_BIT | interrupt.id.0 as usize)
    }

    fn handle_eoi(&mut self, token: DeliveryToken) -> Result<Self::Completion, Self::Error> {
        if token.target.vcpu_id != self.vcpu_id
            || token.sequence == 0
            || token.source.controller != axdevice_base::InterruptControllerId::new(0)
        {
            return Err(RiscvVcpuError::InvalidInput);
        }
        // PLIC claim/complete is owned by the shared controller endpoint. The
        // local IMSIC/VSEIP state has no independent guest EOI register here.
        Ok(())
    }

    fn save_exit(&mut self) -> Result<Self::Completion, Self::Error> {
        Ok(())
    }

    fn reset(&mut self) {
        self.backend.reset_local_interrupts();
    }
}

impl VcpuLocalTimer for AxvmRiscvVcpu {
    type Error = RiscvVcpuError;

    fn arm(&mut self, deadline: u64) -> Result<(), Self::Error> {
        self.backend.program_guest_timer(deadline as usize)
    }

    fn suspend(&mut self) -> Result<(), Self::Error> {
        self.backend.suspend_timer()
    }

    fn resume(&mut self) -> Result<(), Self::Error> {
        self.backend.resume_timer()
    }

    fn cancel(&mut self) -> Result<(), Self::Error> {
        self.backend.cancel_timer()
    }

    fn consume_expiry(&mut self) -> bool {
        self.backend.consume_timer_expiry()
    }
}

impl VmArchVcpuOps for AxvmRiscvVcpu {
    type CreateConfig = RiscvVcpuCreateConfig;
    type SetupConfig = ();
    type Exit = ax_cpu::virtualization::Exit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        riscv_result(RiscvVcpu::new(vm_id, vcpu_id, config))
            .map(|backend| Self { backend, vcpu_id })
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

    fn run(&mut self) -> BackendResult<Self::Exit> {
        riscv_result(self.backend.run_machine())
    }

    fn bind(&mut self) -> BackendResult {
        riscv_result(self.backend.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        riscv_result(self.backend.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.backend.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        riscv_result(self.backend.inject_interrupt(vector))
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
                riscv_result(self.backend.inject_interrupt(vector))
            }
        }
    }

    fn set_return_value(&mut self, val: usize) {
        self.backend.set_return_value(val);
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
