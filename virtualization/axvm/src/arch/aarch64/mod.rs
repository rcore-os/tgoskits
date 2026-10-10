//! AxVM AArch64 adapter.
//!
//! This module owns VM policy and ArceOS integration above `ax_cpu::virtualization`.
//! Guest interrupt state belongs to one VM-local [`arm_vgic::VgicCore`];
//! host IRQ tokens remain opaque until that controller completes split EOI.

mod policy;

use std::sync::Arc;

use arm_vgic::{GicV3Native, GicV3VcpuBinding, IntId};
use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult, *};

use crate::{
    arch::aarch64::policy::*,
    architecture::{
        ArchOps, HypercallExit, MmioReadExit, MmioWriteExit, handle_hypercall,
        ops::{CpuOn, RegisterCompletion},
        sysreg::{self, SysRegReadExit, SysRegWriteExit},
    },
    ax_err,
    engine::{VcpuAction, WaitReason},
    guest_memory::GuestMemoryPort,
    irq::model::{DeliveryToken, PendingVcpuInterrupt, VcpuLocalInterrupts, VcpuLocalTimer},
    runtime::{
        QueuedVcpuInterrupt,
        hvc::{GuestRequest, HyperCallAbi},
    },
    services::{RunServices, RunSignals, VcpuWait},
    vm::{AxVM, AxVMResources},
    *,
};

mod capabilities;
pub(crate) mod fdt;
mod firmware_plan;
mod gic;
pub(super) use gic::prepare as prepare_host_virtualization;
mod npt;
mod resource_pools;
mod shared_provider;
mod vgic;
mod vm;
mod vm_plan;
pub(crate) use vm_plan::Aarch64VmPlan;
mod vtimer;

use vgic::Aarch64VgicRuntimeKey;

pub(crate) struct Aarch64Arch;

/// Binds one run's guest-memory capability into the architecture hardware that
/// needs scoped copies of guest-owned tables.
///
/// The control owner calls this after `AxVM::prepare` has built the device graph
/// and after the run's `GuestMemoryPort` exists, but before
/// [`ArchOps::prepare_entry`] extracts the hardware entry. The capability stays
/// in the task-only ITS adapter inside the VGIC service. The vCPU backend
/// retains only the native controller; the only operation consuming this capability is
/// the task-context GITS MMIO write that drains the command queue with the
/// backend unloaded; the IRQ-reachable controller callbacks never reach it.
pub(crate) fn bind_task_memory(resources: &AxVMResources, memory: GuestMemoryPort) -> AxVmResult {
    let devices = resources.devices()?;
    let runtime = devices
        .services()
        .require::<Aarch64VgicRuntimeKey>()
        .map_err(|error| crate::AxVmError::device("locate AArch64 VGIC runtime", error))?;
    runtime.bind_task_memory(memory)
}

/// Owned register effect produced by one interpreted backend exit.
///
/// AArch64 has no architecture-specific completion register, so the concrete
/// value wraps the shared `RegisterCompletion` and may additionally advance the
/// saved guest PC. The advance is part of the completion, not the fault decoder,
/// so a nested page fault can retry the unchanged instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Aarch64Completion {
    /// Commit the wrapped register effect.
    Register(RegisterCompletion),
    /// Commit `register` and then advance the guest PC by `step` bytes past the
    /// successfully emulated data-abort instruction.
    RegisterThenAdvance {
        register: RegisterCompletion,
        step: usize,
    },
}

impl Default for Aarch64Completion {
    fn default() -> Self {
        Self::Register(RegisterCompletion::None)
    }
}

impl From<RegisterCompletion> for Aarch64Completion {
    fn from(completion: RegisterCompletion) -> Self {
        Self::Register(completion)
    }
}

/// Owned AArch64 VM exit produced after the backend has been unloaded.
///
/// Portable device exits keep the shared owned records. Only the GIC
/// CPU-interface read, which must be resolved while the backend is still
/// loaded, captures a register effect directly.
#[derive(Debug)]
pub(crate) enum Aarch64Exit {
    /// The guest issued an `HVC`/`SMC` hypercall.
    Hypercall(HypercallExit),
    /// The guest read guest physical memory that no stage-2 mapping covers.
    ///
    /// `step` is the faulting instruction length: a device completion resumes
    /// after the access, while a nested-fault fallback leaves the PC unchanged.
    MmioRead { access: MmioReadExit, step: usize },
    /// The guest wrote guest physical memory that no stage-2 mapping covers.
    ///
    /// See [`Aarch64Exit::MmioRead`] for the `step` contract.
    MmioWrite { access: MmioWriteExit, step: usize },
    /// The guest read a trapped system register.
    SysRegRead(SysRegReadExit),
    /// The guest wrote a trapped system register.
    SysRegWrite(SysRegWriteExit),
    /// A GIC CPU-interface read captured while the backend was still loaded.
    GprRead { register: usize, value: u64 },
    /// The guest executed `WFI`/`WFE`.
    WaitForInterrupt,
    /// The vCPU handled the event internally.
    Nothing,
}

impl ArchOps for Aarch64Arch {
    type VCpu = AxvmArmVcpu;
    type PerCpu = AxvmArmPerCpu;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;
    type Entry = ();
    type Exit = Aarch64Exit;
    type Completion = Aarch64Completion;

    fn has_hardware_support() -> bool {
        crate::arch::aarch64::policy::has_hardware_support()
    }

    fn invalidate_translations(
        _entry: &Self::Entry,
        _old_root: axvm_types::NestedPagingConfig,
    ) -> AxVmResult {
        // The control owner unloads every vCPU and calls this once per physical
        // CPU that could still cache the retired stage-2 root. The
        // inner-shareable broadcast retires all guest stage-1/stage-2 entries in
        // the shareable domain and waits for completion, so it does not depend
        // on any vCPU still holding the old VTTBR/VTCR context.
        //
        // SAFETY: AxVM owns the stage-two tables at EL2 and serializes
        // descriptor mutation. Every vCPU has already been unloaded, new guest
        // access is closed, and the retired backing is retained until the new
        // root is published.
        unsafe { ax_cpu::virtualization::invalidate_guest_translations_inner_shareable() };
        Ok(())
    }

    fn prepare_entry(
        resources: &AxVMResources,
        signals: Arc<RunSignals>,
    ) -> AxVmResult<Self::Entry> {
        let devices = resources.devices()?;
        let runtime = devices
            .services()
            .require::<Aarch64VgicRuntimeKey>()
            .map_err(|error| crate::AxVmError::device("locate AArch64 VGIC runtime", error))?;
        // Seal the run identity before any vCPU can run: a controller that is
        // already bound to a different run refuses the new binding, so a stale
        // port can never re-target a fresh runtime.
        runtime.bind_run(&signals)?;
        Ok(())
    }

    fn enter_runtime(vm: &mut AxVM, _signals: &Arc<RunSignals>) -> AxVmResult {
        vgic_runtime(vm)?.activate()
    }

    fn exit_runtime(vm: &mut AxVM, signals: &Arc<RunSignals>) -> AxVmResult {
        let runtime = vgic_runtime(vm)?;
        // Keep the run binding while deactivation is retryable. IRQ routes
        // remain installed when teardown is rejected, and their wake target
        // must stay valid until the physical bindings are retired.
        runtime.deactivate()?;
        // The binding is cleared only for the run that owns it; a retired run
        // can never release or re-target the service used by a newer run.
        runtime.unbind_run(signals.run_id())
    }

    fn prepare_vcpu(vcpu: &mut Self::VCpu, _entry: &Self::Entry) -> AxVmResult {
        // Task-side preparation runs before CPU binding and IRQ masking, so it
        // may discard any timer wait that a previous migration left armed.
        let arch = vcpu;
        let binding = arch
            .timer_binding
            .as_ref()
            .ok_or(crate::AxVmError::Backend {
                operation: "prepare architectural timer",
                source: BackendError::InvalidState,
            })?;
        binding.disarm_wait().map_err(|source| {
            crate::AxVmError::interrupt_controller("disarm architectural timer", source)
        })?;
        binding.prepare_run().map_err(|source| {
            crate::AxVmError::interrupt_controller("retire architectural timer activation", source)
        })
    }

    fn suspend_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        // Guest registers remain owned and saved. Only host producers, line
        // levels and the banked physical activation become quiescent here.
        VcpuLocalTimer::suspend(vcpu).map_err(|source| crate::AxVmError::Backend {
            operation: "quiesce architectural timer",
            source,
        })
    }

    fn resume_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        VcpuLocalTimer::resume(vcpu).map_err(|source| crate::AxVmError::Backend {
            operation: "resume architectural timer",
            source,
        })
    }

    fn quiet_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        Self::suspend_vcpu(vcpu)
    }

    fn entry_cpu_is_ready(vcpu: &mut Self::VCpu) -> bool {
        vcpu.timer_binding
            .as_ref()
            .is_none_or(|binding| binding.entry_cpu_is_ready())
    }

    fn before_guest(vcpu: &mut Self::VCpu, _vcpu_id: usize, _entry: &Self::Entry) -> AxVmResult {
        // Only canonical timer levels are published while the backend is
        // loaded; host cancellation and remote completion ran before CPU pin.
        VcpuLocalInterrupts::prepare_entry(vcpu)
            .map(|_| ())
            .map_err(|source| crate::AxVmError::Backend {
                operation: "prepare AArch64 local interrupt state",
                source,
            })
    }

    fn inject_vcpu_interrupt(vcpu: &mut Self::VCpu, interrupt: PendingVcpuInterrupt) -> AxVmResult {
        VcpuLocalInterrupts::inject(vcpu, interrupt).map_err(|source| crate::AxVmError::Backend {
            operation: "inject AArch64 local interrupt",
            source,
        })
    }

    fn complete(
        vcpu: &mut Self::VCpu,
        _entry: &Self::Entry,
        completion: Self::Completion,
    ) -> AxVmResult {
        let (register, advance) = match completion {
            Aarch64Completion::Register(register) => (register, None),
            Aarch64Completion::RegisterThenAdvance { register, step } => (register, Some(step)),
        };
        match register {
            RegisterCompletion::None => {}
            RegisterCompletion::Gpr { register, value } => vcpu.set_gpr(register, value),
            RegisterCompletion::Return(value) => vcpu.set_return_value(value),
        }
        if let Some(step) = advance {
            vcpu.advance_exception_pc(step);
        }
        Ok(())
    }

    fn capture_exit(
        vcpu: &mut Self::VCpu,
        _entry: &Self::Entry,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<Self::Exit> {
        // This hook runs while the backend is still loaded on its owning CPU, so
        // every GIC CPU-interface register effect and locally trapped system
        // register is resolved here without a second backend bind.
        Ok(match exit {
            ArmVmExit::Hypercall { nr, args } => Aarch64Exit::Hypercall(HypercallExit { nr, args }),
            ArmVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
                step,
            } => Aarch64Exit::MmioRead {
                access: MmioReadExit {
                    addr: arm_guest_phys_addr_to_ax(addr),
                    width: arm_access_width_to_ax(width),
                    reg,
                    reg_width: arm_access_width_to_ax(reg_width),
                    signed_ext,
                },
                step,
            },
            ArmVmExit::MmioWrite {
                addr,
                width,
                data,
                step,
            } => Aarch64Exit::MmioWrite {
                access: MmioWriteExit {
                    addr: arm_guest_phys_addr_to_ax(addr),
                    width: arm_access_width_to_ax(width),
                    data,
                },
                step,
            },
            ArmVmExit::SysRegRead { addr, reg } => Aarch64Exit::SysRegRead(SysRegReadExit {
                addr: arm_sys_reg_addr_to_ax(addr),
                reg,
            }),
            ArmVmExit::SysRegWrite { addr, value } => Aarch64Exit::SysRegWrite(SysRegWriteExit {
                addr: arm_sys_reg_addr_to_ax(addr),
                value,
            }),
            ArmVmExit::GicCpuInterfaceRead {
                register,
                destination,
            } => {
                let value = vcpu.read_icc(register)?;
                Aarch64Exit::GprRead {
                    register: destination,
                    value,
                }
            }
            ArmVmExit::GicCpuInterfaceWrite { register, value } => {
                vcpu.write_icc(register, value)?;
                Aarch64Exit::Nothing
            }
            ArmVmExit::SendIPI { value } => {
                vcpu.write_sgi1r(value)?;
                Aarch64Exit::Nothing
            }
            ArmVmExit::DeactivateInterrupt { intid } => {
                vcpu.deactivate(intid)?;
                Aarch64Exit::Nothing
            }
            ArmVmExit::WaitForInterrupt => Aarch64Exit::WaitForInterrupt,
            ArmVmExit::Nothing => Aarch64Exit::Nothing,
        })
    }

    fn handle_exit(
        exit: Self::Exit,
        vcpu_id: usize,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<Self::Completion, GuestRequest>> {
        // The backend is already unloaded and host IRQs restored; only owned
        // records reach this interpreter, so it may use sleepable runtime and
        // device services without holding a hardware binding.
        match exit {
            Aarch64Exit::Hypercall(exit) => {
                handle_hypercall::<Self>(services, vcpu_id, exit, HyperCallAbi::native())
            }
            Aarch64Exit::MmioRead { access, step } => {
                match crate::architecture::exit::try_handle_mmio_read(services, vcpu_id, access)? {
                    Some(completion) => Ok(VcpuAction::Reenter(
                        Aarch64Completion::RegisterThenAdvance {
                            register: completion,
                            step,
                        },
                    )),
                    // No device owns the address, so the original stage-2
                    // translation fault must be satisfiable by the control owner
                    // before the guest retries the very same instruction.
                    None => Ok(VcpuAction::Control(GuestRequest::NestedFault {
                        addr: access.addr,
                        access_flags: MappingFlags::READ,
                    })),
                }
            }
            Aarch64Exit::MmioWrite { access, step } => {
                if !crate::architecture::exit::try_handle_mmio_write(services, vcpu_id, access)? {
                    return Ok(VcpuAction::Control(GuestRequest::NestedFault {
                        addr: access.addr,
                        access_flags: MappingFlags::WRITE,
                    }));
                }
                Ok(VcpuAction::Reenter(
                    Aarch64Completion::RegisterThenAdvance {
                        register: RegisterCompletion::None,
                        step,
                    },
                ))
            }
            Aarch64Exit::SysRegRead(exit) => sysreg::handle_read::<Self>(services, vcpu_id, exit),
            Aarch64Exit::SysRegWrite(exit) => sysreg::handle_write::<Self>(services, vcpu_id, exit),
            Aarch64Exit::GprRead { register, value } => Ok(VcpuAction::Reenter(
                RegisterCompletion::Gpr {
                    register,
                    value: value as usize,
                }
                .into(),
            )),
            Aarch64Exit::WaitForInterrupt => {
                Ok(VcpuAction::Wait(WaitReason { return_value: None }))
            }
            Aarch64Exit::Nothing => Ok(VcpuAction::Reenter(Aarch64Completion::default())),
        }
    }

    fn inject_arch_interrupt(
        _vcpu: &mut Self::VCpu,
        _vcpu_id: usize,
        _entry: &Self::Entry,
        _interrupt: QueuedVcpuInterrupt,
    ) -> AxVmResult {
        // AArch64 delivers every queued interrupt through the VM-local VGIC;
        // there is no separate architecture-only interrupt class.
        Err(crate::AxVmError::Backend {
            operation: "inject AArch64 architecture interrupt",
            source: BackendError::Unsupported,
        })
    }

    fn wait_for_event(
        vcpu: &mut Self::VCpu,
        _vcpu_id: usize,
        _entry: &Self::Entry,
        wait: &VcpuWait,
    ) -> AxVmResult {
        // The predicate uses only lower controller state and the pre-bound
        // timer-wait completion token; it never queries the VM, its devices, or
        // a sleepable lifecycle lock.
        let arch: &AxvmArmVcpu = &*vcpu;
        if arch.has_pending_interrupt()? {
            return Ok(());
        }
        let timer_wait = arch.arm_timer_wait()?;
        if arch.has_pending_interrupt()? {
            return Ok(());
        }
        wait.wait_until(|| {
            arch.has_pending_interrupt().unwrap_or(false)
                || timer_wait.is_some_and(|token| arch.timer_wait_completed(token))
        });
        Ok(())
    }
}

impl CpuOn for Aarch64Arch {
    fn initialize_cpu_on(
        vcpu: &mut Self::VCpu,
        entry: axvm_types::GuestPhysAddr,
        argument: usize,
    ) -> AxVmResult {
        // The control owner resolved the PSCI topology and reserved this target.
        // The target owner installs the guest entry point and the handoff
        // context in x0 before the first entry; bind/rollback stays in the
        // common vCPU owner.
        vcpu.set_entry(entry)
            .map_err(|error| crate::vcpu::map_vcpu_backend_error("set PSCI CPU_ON entry", error))?;
        vcpu.set_gpr(0, argument);
        Ok(())
    }
}

fn vgic_runtime(vm: &crate::AxVM) -> AxVmResult<Arc<vgic::Aarch64VgicRuntime>> {
    Ok(vm
        .get_devices()?
        .services()
        .require::<Aarch64VgicRuntimeKey>()?)
}

struct HostGuestTrap;

#[trait_ffi::impl_extern_trait]
impl ax_cpu::virtualization::GuestHostTrap for HostGuestTrap {
    fn current_irq(_context: ax_cpu::trap::InterruptedContext) {
        if let Some(token) = gic::acknowledge_host_irq()
            && let Err(error) = gic::route_acknowledged_host_irq(token)
        {
            warn!("{error}");
        }
    }
}

/// One owned AArch64 vCPU backend.
///
/// The value is moved between host CPUs only while no guest entry is in
/// progress. It deliberately retains no CPU-banked GIC state: the per-vCPU VGIC
/// CPU-interface registers are loaded from the controller and saved back inside
/// the single `run` call, which executes under `AxVCpu`'s exclusive, CPU-pinned
/// backend scope with local IRQs masked. Between entries the canonical
/// pending/active/LR state stays in the controller, so `bind`/`unbind` are
/// no-ops and there is exactly one live-bank window, owned by `run`.
///
/// This is what makes the required `Send` sound: the type holds no raw CPU-bank
/// handle or self-referential pointer, and moving it cannot move a live bank
/// because a live bank only ever exists inside the pinned `run` scope. No
/// `unsafe impl Send`/`Sync` is added.
pub(crate) struct AxvmArmVcpu {
    inner: ArmVcpu,
    vcpu_id: usize,
    vgic: Option<GicV3Native>,
    vgic_binding: Option<GicV3VcpuBinding>,
    timer_binding: Option<Arc<vtimer::Aarch64TimerBinding>>,
}

impl AxvmArmVcpu {
    pub(crate) fn attach_vgic(
        &mut self,
        vgic: GicV3Native,
        irq_binding: vgic::Aarch64VcpuIrqBinding,
        timer_config: crate::arch::aarch64::policy::ArmTimerVmConfig,
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
            vgic.clone(),
            backend,
            binding.vcpu(),
            virtual_timer_ppi,
            physical_timer_ppi,
            host_virtual_timer_intid,
            timer_config.frequency(),
        )
        .map_err(|error| {
            crate::AxVmError::interrupt_controller("bind host virtual-timer PPI", error)
        })?;
        self.vgic = Some(vgic);
        self.vgic_binding = Some(binding);
        self.timer_binding = Some(timer_binding);
        Ok(())
    }

    fn binding(&self) -> AxVmResult<&GicV3VcpuBinding> {
        self.vgic_binding.as_ref().ok_or(crate::AxVmError::Backend {
            operation: "locate VGIC vCPU binding",
            source: BackendError::InvalidState,
        })
    }

    /// Advances the saved guest PC past one emulated faulting instruction.
    ///
    /// Only the successful device completion calls this; a nested page fault
    /// leaves the PC untouched so the guest retries the same instruction.
    fn advance_exception_pc(&mut self, step: usize) {
        self.inner.advance_exception_pc(step);
    }

    fn write_sgi1r(&self, value: u64) -> AxVmResult {
        self.binding()?
            .write_sgi1r(value)
            .map_err(|error| crate::AxVmError::interrupt_controller("write ICC_SGI1R_EL1", error))
    }

    fn read_icc(&self, register: ArmGicCpuInterfaceRegister) -> AxVmResult<u64> {
        let binding = self.binding()?;
        let result = match register {
            ArmGicCpuInterfaceRegister::Control => binding.read_icc_control(),
            ArmGicCpuInterfaceRegister::PriorityMask => binding.read_icc_priority_mask(),
            ArmGicCpuInterfaceRegister::RunningPriority => binding.read_icc_running_priority(),
        };
        result.map_err(|error| {
            crate::AxVmError::interrupt_controller("read virtual ICC register", error)
        })
    }

    fn write_icc(&self, register: ArmGicCpuInterfaceRegister, value: u64) -> AxVmResult {
        let binding = self.binding()?;
        let result = match register {
            ArmGicCpuInterfaceRegister::Control => binding.write_icc_control(value),
            ArmGicCpuInterfaceRegister::PriorityMask => binding.write_icc_priority_mask(value),
            ArmGicCpuInterfaceRegister::RunningPriority => Ok(()),
        };
        result.map_err(|error| {
            crate::AxVmError::interrupt_controller("write virtual ICC register", error)
        })
    }

    fn deactivate(&self, intid: u32) -> AxVmResult {
        let intid = IntId::new(intid).map_err(|error| {
            crate::AxVmError::interrupt_controller("validate ICC_DIR_EL1 INTID", error)
        })?;
        self.binding()?.deactivate_saved(intid).map_err(|error| {
            crate::AxVmError::interrupt_controller("deactivate virtual interrupt", error)
        })
    }

    fn synchronize_timer(&self) -> BackendResult {
        let snapshot = arm_result(self.inner.timer_snapshot())?;
        let binding = self
            .timer_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?;
        vgic_backend_result(binding.synchronize(snapshot))
    }

    fn arm_timer_wait(&self) -> AxVmResult<Option<vtimer::Aarch64TimerWaitToken>> {
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
            .map_err(|error| {
                crate::AxVmError::interrupt_controller("arm architectural timer wait", error)
            })
    }

    fn timer_wait_completed(&self, token: vtimer::Aarch64TimerWaitToken) -> bool {
        self.timer_binding
            .as_ref()
            .is_some_and(|binding| binding.timer_wait_completed(token))
    }

    fn prepare_timer_entry(&self) -> AxVmResult {
        let binding = self
            .timer_binding
            .as_ref()
            .ok_or(crate::AxVmError::Backend {
                operation: "locate architectural timer binding",
                source: BackendError::InvalidState,
            })?;
        let snapshot = self.inner.timer_snapshot().map_err(|error| {
            crate::vcpu::map_vcpu_backend_error(
                "snapshot AArch64 architectural timers before entry",
                arm_error_to_backend(error),
            )
        })?;
        binding.publish_for_entry(snapshot).map_err(|error| {
            crate::AxVmError::interrupt_controller("publish timer PPI before entry", error)
        })
    }

    fn accept_host_timer_irq(&self, token: usize) -> bool {
        self.timer_binding
            .as_ref()
            .is_some_and(|binding| binding.accept_host_irq(token))
    }

    fn has_pending_interrupt(&self) -> AxVmResult<bool> {
        self.binding()?.has_pending_interrupt().map_err(|error| {
            crate::AxVmError::interrupt_controller("query pending virtual interrupt", error)
        })
    }
}

impl VmArchVcpuOps for AxvmArmVcpu {
    type CreateConfig = ArmVcpuCreateConfig;
    type SetupConfig = ArmVcpuSetupConfig;
    type Exit = ArmVmExit;

    fn guest_mpidr_from_create_config(config: &Self::CreateConfig) -> Option<u64> {
        Some(config.mpidr_el1)
    }

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        arm_result(ArmVcpu::new(vm_id, vcpu_id, config)).map(|inner| Self {
            inner,
            vcpu_id,
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
        let save_result = VcpuLocalInterrupts::save_exit(self);
        // IRQ tokens are CPU-local resources, not durable guest exits. Resolve
        // them even when timer/VGIC saving fails, while the original IRQ mask
        // and CPU binding are still held.
        let run_result = run_result.and_then(|exit| match exit {
            ArmRunExit::Guest(exit) => Ok(exit),
            ArmRunExit::HostInterrupt(token) => {
                if let Some(token) = token
                    && !self.accept_host_timer_irq(token)
                {
                    gic::route_acknowledged_host_irq(token).map_err(|error| {
                        error!("failed to route acknowledged host IRQ: {error:?}");
                        BackendError::InvalidState
                    })?;
                }
                Ok(ArmVmExit::Nothing)
            }
        });
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
        // No CPU bank is retained between entries, so the architecture load
        // boundary is the guest entry itself (`run`). See `AxvmArmVcpu` for why
        // this keeps the required `Send` sound without `unsafe impl`.
        arm_result(self.inner.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        // Counterpart of `bind`; `run` has already saved the VGIC CPU-interface
        // state under the same IRQ-masked, CPU-pinned scope.
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

impl VcpuLocalInterrupts for AxvmArmVcpu {
    type Snapshot = bool;
    type Completion = ();
    type Error = BackendError;

    fn prepare_entry(&mut self) -> Result<Self::Snapshot, Self::Error> {
        self.prepare_timer_entry()
            .map_err(|_| BackendError::InvalidState)?;
        self.has_pending_interrupt()
            .map_err(|_| BackendError::InvalidState)
    }

    fn inject(&mut self, interrupt: PendingVcpuInterrupt) -> Result<(), Self::Error> {
        self.inject_interrupt_with_trigger(interrupt.id.0 as usize, interrupt.trigger)
    }

    fn handle_eoi(&mut self, token: DeliveryToken) -> Result<Self::Completion, Self::Error> {
        if token.target.vcpu_id != self.vcpu_id
            || token.sequence == 0
            || token.source.controller != axdevice_base::InterruptControllerId::new(0)
        {
            return Err(BackendError::InvalidInput);
        }
        self.deactivate(token.source.source)
            .map_err(|_| BackendError::InvalidState)
            .map(|_| ())
    }

    fn save_exit(&mut self) -> Result<Self::Completion, Self::Error> {
        let binding = self
            .vgic_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?;
        vgic_backend_result(binding.save()).map(|_| ())
    }

    fn reset(&mut self) {
        if let Some(binding) = &self.timer_binding {
            let _ = binding.reset();
        }
    }
}

impl VcpuLocalTimer for AxvmArmVcpu {
    type Error = BackendError;

    fn arm(&mut self, _deadline: u64) -> Result<(), Self::Error> {
        self.timer_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?
            .prepare_run()
            .map_err(|_| BackendError::InvalidState)
    }

    fn suspend(&mut self) -> Result<(), Self::Error> {
        self.timer_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?
            .reset()
            .map_err(|_| BackendError::InvalidState)
    }

    fn resume(&mut self) -> Result<(), Self::Error> {
        self.timer_binding
            .as_ref()
            .ok_or(BackendError::InvalidState)?
            .prepare_run()
            .map_err(|_| BackendError::InvalidState)
    }

    fn cancel(&mut self) -> Result<(), Self::Error> {
        self.suspend()
    }

    fn consume_expiry(&mut self) -> bool {
        self.timer_binding
            .as_ref()
            .is_some_and(|binding| binding.consume_expiry())
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
        arm_result(self.0.hardware_enable())?;
        if let Err(error) = gic::enable_current_cpu() {
            if let Err(rollback_error) = self.0.hardware_disable() {
                warn!(
                    "failed to roll back AArch64 virtualization after host GIC setup failed: \
                     {rollback_error:?}"
                );
            }
            return Err(error);
        }
        Ok(())
    }

    fn hardware_disable(&mut self) -> BackendResult {
        gic::disable_current_cpu()?;
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
        arm_vgic::VgicError::NativeState { kind, .. } => match kind {
            arm_vgic::StateErrorKind::ResourceBusy => BackendError::ResourceBusy,
            arm_vgic::StateErrorKind::Unsupported => BackendError::Unsupported,
            arm_vgic::StateErrorKind::InvalidInput => BackendError::InvalidInput,
            arm_vgic::StateErrorKind::NotFound | arm_vgic::StateErrorKind::InvalidState => {
                BackendError::InvalidState
            }
        },
        arm_vgic::VgicError::ResourceNotFound { .. }
        | arm_vgic::VgicError::InvalidStateTransition { .. }
        | arm_vgic::VgicError::Backend { .. }
        | arm_vgic::VgicError::HostService { .. }
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
    fn converts_vcpu_policy_errors_to_backend_errors() {
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
