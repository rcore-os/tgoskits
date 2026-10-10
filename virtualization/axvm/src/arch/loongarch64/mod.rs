use std::{boxed::Box, sync::Arc, time::Duration};

use ax_memory_addr::VirtAddr;
use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult, *};
use policy::*;

mod policy;

use super::*;
use crate::{
    AxVmError, AxVmResult,
    architecture::{
        HypercallExit, MmioReadExit, MmioWriteExit, exit as shared_exit, ops::RegisterCompletion,
    },
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

pub(crate) mod boot;
mod capabilities;
pub(crate) mod irq;
mod npt;
mod pci_config;
mod resource_pools;
mod vm;
pub(crate) use vm::LoongArchVmPlan;

pub(crate) struct LoongArch64Arch;

pub(crate) fn apply_interrupt_event(
    devices: &Arc<axdevice::DeviceRuntime>,
    event: crate::irq::model::SourceEvent,
) -> AxVmResult {
    let owner = devices
        .services()
        .require::<irq::LoongArchPchPicRuntimeKey>()
        .map_err(|error| AxVmError::device("resolve LoongArch interrupt owner", error))?;
    crate::irq::model::InterruptControllerOwner::apply_source(owner.as_ref(), event)
        .map_err(|error| AxVmError::interrupt("apply LoongArch interrupt event", error))
}

/// Run-bound native controller state prepared by the control owner.
///
/// The entry holds narrow, immutable per-run values only: the guest-visible
/// PCH-PIC output port and the exact run-bound publication capability. A queued
/// physical source is resolved against pre-bound short controller state instead
/// of a per-exit VM lookup, and no VM handle or sleeping lock is retained.
pub(crate) struct LoongArchEntry {
    pch_pic: Arc<dyn axdevice::PchPicOutputPort>,
    run: irq::LoongArchRunPort,
    physical_inputs: [Option<usize>; irq::LOONGARCH_MAX_IRQ_COUNT],
}

/// Backend exit awaiting the task-context finish stage.
///
/// `capture_exit` snapshots the raw LVZ exit plus the host CPU-local operands the
/// software emulator needs, while the backend is still bound. Software CSR and
/// guest-timer interpretation runs in `finish_exit`, after the engine has
/// unloaded the backend and restored the host CPU and IRQs, so no host timer
/// registration or other sleepable host service runs inside the pinned
/// guest-entry scope and no task-stage read touches the wrong CPU's local state.
pub(crate) enum LoongArchExit {
    /// Raw LVZ exit plus its pinned host-local operand snapshot.
    Machine {
        exit: ax_cpu::virtualization::Exit,
        host: LoongArchPinnedHost,
    },
    /// Fully interpreted, owned record consumed by `handle_exit`.
    Record(LoongArchExitRecord),
}

/// Owned LoongArch exit record plus the run-bound capabilities it needs.
pub(crate) struct LoongArchExitRecord {
    kind: LoongArchExitKind,
    pch_pic: Arc<dyn axdevice::PchPicOutputPort>,
    run: irq::LoongArchRunPort,
}

enum LoongArchExitKind {
    Hypercall {
        nr: u64,
        args: [u64; 6],
    },
    MmioRead {
        addr: GuestPhysAddr,
        width: AccessWidth,
        reg: usize,
        reg_width: AccessWidth,
        signed_ext: bool,
        /// Original fault flags when the access was decoded from a nested page
        /// fault; `None` for a direct MMIO exit that has no page-fault fallback.
        fallback: Option<MappingFlags>,
    },
    MmioWrite {
        addr: GuestPhysAddr,
        width: AccessWidth,
        data: u64,
        /// Original fault flags when the access was decoded from a nested page
        /// fault; `None` for a direct MMIO exit that has no page-fault fallback.
        fallback: Option<MappingFlags>,
    },
    NestedPageFault {
        addr: GuestPhysAddr,
        access_flags: MappingFlags,
    },
    Idle,
    Halt,
    Nothing,
}

/// Guest register effects committed by the vCPU owner before the next entry.
///
/// The instruction PC advances only here: a device access is committed after a
/// device has claimed it, while an unclaimed access falls back to a nested page
/// fault that must re-execute the same instruction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct LoongArchCompletion {
    register: RegisterCompletion,
    advance_pc: bool,
}

impl LoongArchCompletion {
    /// Completion for one device access that a device accepted.
    const fn device(register: RegisterCompletion) -> Self {
        Self {
            register,
            advance_pc: true,
        }
    }
}

impl From<RegisterCompletion> for LoongArchCompletion {
    fn from(register: RegisterCompletion) -> Self {
        Self {
            register,
            advance_pc: false,
        }
    }
}

impl ArchOps for LoongArch64Arch {
    type VCpu = AxvmLoongArchVcpu;
    type PerCpu = AxvmLoongArchPerCpu;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;
    type Entry = LoongArchEntry;
    type Exit = LoongArchExit;
    type Completion = LoongArchCompletion;

    fn has_hardware_support() -> bool {
        ax_cpu::capability::has_hypervisor_extension()
    }

    /// Drops every locally cached guest translation for the retired root.
    ///
    /// The control owner calls this in a synchronous host-CPU rendezvous after
    /// every vCPU has unloaded, once per CPU that could cache the old root. It
    /// performs a real `INVTLB_ALLGID` invalidation instead of relying on the
    /// next guest entry to refresh the translation window.
    fn invalidate_translations(
        _entry: &Self::Entry,
        _old_root: axvm_types::NestedPagingConfig,
    ) -> AxVmResult {
        // SAFETY: the caller guarantees this runs in root mode with local
        // interrupts disabled on an LVZ CPU that has unloaded every guest, so
        // no guest can observe a partially invalidated translation window.
        // `invtlb 0x12` drops all cached guest translations, including entries
        // tagged with another guest id, for this CPU only.
        unsafe { ax_cpu::virtualization::invalidate_guest_translations() };
        Ok(())
    }

    fn prepare_entry(
        resources: &AxVMResources,
        signals: Arc<RunSignals>,
    ) -> AxVmResult<Self::Entry> {
        let run = irq::LoongArchRunPort::new(signals);
        let devices = resources.devices()?;
        let pch_pic = devices
            .services()
            .require::<axdevice::PchPicOutputPortKey>()?;
        // The per-run controller runtime is built with the devices and is bound
        // here, before guest entry is admitted.
        let runtime = devices
            .services()
            .require::<irq::LoongArchPchPicRuntimeKey>()?;
        runtime.activate(run.clone());
        let mut physical_inputs = [None; irq::LOONGARCH_MAX_IRQ_COUNT];
        for route in &resources.device_plan.physical_routes {
            physical_inputs[route.physical_irq] = Some(route.guest_input);
        }
        Ok(Self::Entry {
            pch_pic,
            run,
            physical_inputs,
        })
    }

    fn enter_runtime(vm: &mut crate::AxVM, signals: &Arc<RunSignals>) -> AxVmResult {
        irq::enter_runtime(
            &vm.resources.device_plan.physical_routes,
            &irq::LoongArchRunPort::new(Arc::clone(signals)),
        )
    }

    fn exit_runtime(vm: &mut crate::AxVM, signals: &Arc<RunSignals>) -> AxVmResult {
        irq::exit_runtime(vm.id(), signals.run_id());
        if let Ok(devices) = vm.get_devices()
            && let Ok(runtime) = devices
                .services()
                .require::<irq::LoongArchPchPicRuntimeKey>()
        {
            runtime.deactivate();
        }
        Ok(())
    }

    fn prepare_vcpu(vcpu: &mut Self::VCpu, entry: &Self::Entry) -> AxVmResult {
        vcpu.backend.set_run_port(entry.run.clone());
        Ok(())
    }

    /// Quiesces the guest timer producer while preserving its logical deadline.
    fn suspend_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        VcpuLocalTimer::suspend(vcpu)
            .map_err(|error| AxVmError::vcpu("suspend LoongArch timer", error))
    }

    /// Re-arms a suspended guest timer before guest admission reopens.
    fn resume_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        VcpuLocalTimer::resume(vcpu)
            .map_err(|error| AxVmError::vcpu("resume LoongArch timer", error))
    }

    fn quiet_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        VcpuLocalTimer::cancel(vcpu)
            .map_err(|error| AxVmError::vcpu("quiet LoongArch timer", error))
    }

    fn before_guest(vcpu: &mut Self::VCpu, _vcpu_id: usize, _entry: &Self::Entry) -> AxVmResult {
        VcpuLocalInterrupts::prepare_entry(vcpu)
            .map_err(|error| AxVmError::vcpu("prepare LoongArch local interrupt state", error))?;
        Ok(())
    }

    fn inject_vcpu_interrupt(vcpu: &mut Self::VCpu, interrupt: PendingVcpuInterrupt) -> AxVmResult {
        VcpuLocalInterrupts::inject(vcpu, interrupt)
            .map_err(|error| AxVmError::vcpu("inject LoongArch local interrupt", error))
    }

    fn complete(
        vcpu: &mut Self::VCpu,
        _entry: &Self::Entry,
        completion: Self::Completion,
    ) -> AxVmResult {
        match completion.register {
            RegisterCompletion::None => {}
            RegisterCompletion::Gpr { register, value } => vcpu.set_gpr(register, value),
            RegisterCompletion::Return(value) => vcpu.set_return_value(value),
        }
        if completion.advance_pc {
            vcpu.advance_guest_pc();
        }
        Ok(())
    }

    fn capture_exit(
        vcpu: &mut Self::VCpu,
        entry: &Self::Entry,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<Self::Exit> {
        match exit {
            // The LVZ backend reports every guest exit through `run_machine`.
            // Only the raw record plus the pinned host-local operands are taken
            // here; `finish_exit` owns the software interpretation.
            LoongArchVmExit::Machine(machine_exit) => {
                let host = vcpu.capture_pinned_host(&machine_exit);
                Ok(LoongArchExit::Machine {
                    exit: machine_exit,
                    host,
                })
            }
            // A pre-decoded exit produced outside the LVZ interpreter path is
            // already durable; no further backend work is pending for it.
            decoded => {
                let kind = interpret_loongarch_exit(vcpu, decoded)?;
                Ok(LoongArchExit::Record(LoongArchExitRecord {
                    kind,
                    pch_pic: Arc::clone(&entry.pch_pic),
                    run: entry.run.clone(),
                }))
            }
        }
    }

    /// Interprets software-only exits after the backend has been unloaded.
    ///
    /// The engine calls this outside `with_engine_scope`, so guest CSR
    /// emulation and its `H::register_timer`/`H::cancel_timer` calls run in
    /// plain task context instead of inside the pinned guest-entry scope. Every
    /// host CPU-local read and the native IOCSR passthrough write were resolved
    /// while the backend was pinned and are carried in `LoongArchPinnedHost`, so
    /// this stage never reads or writes the CPUCFG or IOCSR bank of the CPU it
    /// happens to run on. MMIO decoding still reads only the saved guest context
    /// and faulting instruction.
    fn finish_exit(
        vcpu: &mut Self::VCpu,
        entry: &Self::Entry,
        exit: Self::Exit,
    ) -> AxVmResult<Self::Exit> {
        let (machine_exit, pinned) = match exit {
            LoongArchExit::Machine {
                exit: machine_exit,
                host,
            } => (machine_exit, host),
            // Already durable; nothing further to interpret.
            record => return Ok(record),
        };
        let backend = vcpu;
        let interpreted = backend
            .backend
            .process_exit(machine_exit, pinned)
            .map_err(|error| AxVmError::vcpu("interpret LVZ exit", error))?;
        let kind = interpret_loongarch_exit(backend, interpreted)?;
        Ok(LoongArchExit::Record(LoongArchExitRecord {
            kind,
            pch_pic: Arc::clone(&entry.pch_pic),
            run: entry.run.clone(),
        }))
    }

    fn handle_exit(
        exit: Self::Exit,
        vcpu_id: usize,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<Self::Completion, GuestRequest>> {
        let LoongArchExit::Record(LoongArchExitRecord { kind, pch_pic, run }) = exit else {
            return Err(AxVmError::vcpu(
                "interpret LVZ exit",
                "exit reached the handler without the task-context finish stage",
            ));
        };
        match kind {
            LoongArchExitKind::Hypercall { nr, args } => {
                // The shared exit interpreter already produces this
                // architecture's completion type.
                shared_exit::handle_hypercall::<Self>(
                    services,
                    vcpu_id,
                    HypercallExit { nr, args },
                    HyperCallAbi::native(),
                )
            }
            LoongArchExitKind::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
                fallback,
            } => {
                let access = MmioReadExit {
                    addr,
                    width,
                    reg,
                    reg_width,
                    signed_ext,
                };
                match fallback {
                    Some(access_flags) => {
                        let Some(completion) =
                            shared_exit::try_handle_mmio_read(services, vcpu_id, access)?
                        else {
                            // The fault decoded as a load but no device owns the
                            // address: hand it back as a nested page fault so the
                            // control owner can map guest memory lazily.
                            return Ok(VcpuAction::Control(GuestRequest::NestedFault {
                                addr,
                                access_flags,
                            }));
                        };
                        publish_pch_pic_outputs(&pch_pic, &run)?;
                        Ok(VcpuAction::Reenter(LoongArchCompletion::device(completion)))
                    }
                    None => {
                        // A direct device access has no page-fault retry, so the
                        // shared register effect is retired immediately.
                        let action =
                            shared_exit::handle_mmio_read::<Self>(services, vcpu_id, access)?;
                        publish_pch_pic_outputs(&pch_pic, &run)?;
                        Ok(retire_device_action(action))
                    }
                }
            }
            LoongArchExitKind::MmioWrite {
                addr,
                width,
                data,
                fallback,
            } => {
                let access = MmioWriteExit { addr, width, data };
                match fallback {
                    Some(access_flags) => {
                        if !shared_exit::try_handle_mmio_write(services, vcpu_id, access)? {
                            // No device accepted the decoded store: report the
                            // original nested page fault to the control owner.
                            return Ok(VcpuAction::Control(GuestRequest::NestedFault {
                                addr,
                                access_flags,
                            }));
                        }
                        publish_pch_pic_outputs(&pch_pic, &run)?;
                        Ok(VcpuAction::Reenter(LoongArchCompletion::device(
                            RegisterCompletion::None,
                        )))
                    }
                    None => {
                        let action =
                            shared_exit::handle_mmio_write::<Self>(services, vcpu_id, access)?;
                        publish_pch_pic_outputs(&pch_pic, &run)?;
                        Ok(retire_device_action(action))
                    }
                }
            }
            LoongArchExitKind::NestedPageFault { addr, access_flags } => {
                Ok(VcpuAction::Control(GuestRequest::NestedFault {
                    addr,
                    access_flags,
                }))
            }
            LoongArchExitKind::Idle => {
                trace!("VCpu[{vcpu_id}] LoongArch idle");
                Ok(VcpuAction::Wait(WaitReason { return_value: None }))
            }
            LoongArchExitKind::Halt => {
                debug!("VCpu[{vcpu_id}] LoongArch halt");
                Ok(VcpuAction::Wait(WaitReason { return_value: None }))
            }
            LoongArchExitKind::Nothing => Ok(VcpuAction::Reenter(LoongArchCompletion::default())),
        }
    }

    fn inject_arch_interrupt(
        vcpu: &mut Self::VCpu,
        vcpu_id: usize,
        entry: &Self::Entry,
        interrupt: QueuedVcpuInterrupt,
    ) -> AxVmResult {
        let (vector, physical_irq) = match interrupt {
            QueuedVcpuInterrupt::Physical {
                vector,
                physical_irq,
            } => (vector, Some(physical_irq)),
            QueuedVcpuInterrupt::External { vector } => (vector, None),
            QueuedVcpuInterrupt::Virtual(_) => {
                return Err(AxVmError::unsupported(
                    "inject LoongArch interrupt",
                    "virtual interrupt reached the architecture queue",
                ));
            }
        };
        let Some(physical_irq) = physical_irq else {
            vcpu.inject_eiointc_interrupt(vector).map_err(|error| {
                AxVmError::interrupt("inject LoongArch EIOINTC interrupt", error)
            })?;
            return Ok(());
        };

        // Physical GSIs and guest PCH-PIC inputs are separate namespaces.
        // The immutable route preserves the source identity even when guest
        // controller programming changes its output vector.
        let input = entry
            .physical_inputs
            .get(physical_irq)
            .copied()
            .flatten()
            .ok_or_else(|| {
                AxVmError::invalid_config(
                    "queued LoongArch physical source has no prepared guest input",
                )
            })?;
        let Some(vector) = entry.pch_pic.set_input_level(input, true) else {
            trace!(
                "Queued LoongArch external interrupt physical_irq={physical_irq:#x} is masked in \
                 VCpu[{}]",
                vcpu_id
            );
            return Ok(());
        };
        trace!(
            "Injecting queued LoongArch external interrupt vector={vector:#x}, \
             physical_irq={physical_irq:#x} into VCpu[{}]",
            vcpu_id
        );
        vcpu.inject_external_interrupt(vector, physical_irq)
            .map_err(|error| AxVmError::interrupt("inject LoongArch external interrupt", error))?;
        Ok(())
    }

    fn wait_for_event(
        vcpu: &mut Self::VCpu,
        _vcpu_id: usize,
        _entry: &Self::Entry,
        wait: &VcpuWait,
    ) -> AxVmResult {
        // The predicate only reads this vCPU's own pending-interrupt state, so
        // the backend borrow is hoisted out and shared by the `Fn` closure.
        let backend = vcpu;
        wait.wait_until(|| backend.has_enabled_pending_interrupt());
        Ok(())
    }
}

fn interpret_loongarch_exit(
    backend: &mut AxvmLoongArchVcpu,
    exit: LoongArchVmExit,
) -> AxVmResult<LoongArchExitKind> {
    match exit {
        // The LVZ interpreter never nests a raw machine exit inside an
        // interpreted one; `finish_exit` unwraps it before this runs.
        LoongArchVmExit::Machine(_) => Err(AxVmError::vcpu(
            "interpret LVZ exit",
            "nested LVZ machine exit reached the software interpreter",
        )),
        LoongArchVmExit::Hypercall { nr, args } => Ok(LoongArchExitKind::Hypercall { nr, args }),
        LoongArchVmExit::MmioRead {
            addr,
            width,
            reg,
            reg_width,
            signed_ext,
        } => Ok(LoongArchExitKind::MmioRead {
            addr: loong_guest_phys_addr_to_ax(addr),
            width: loong_access_width_to_ax(width),
            reg,
            reg_width: loong_access_width_to_ax(reg_width),
            signed_ext,
            fallback: None,
        }),
        LoongArchVmExit::MmioWrite { addr, width, data } => Ok(LoongArchExitKind::MmioWrite {
            addr: loong_guest_phys_addr_to_ax(addr),
            width: loong_access_width_to_ax(width),
            data,
            fallback: None,
        }),
        LoongArchVmExit::NestedPageFault { addr, access_flags } => {
            let fault_flags = loong_access_flags_to_ax(access_flags);
            if let Some(decoded) = backend.decode_mmio_fault(addr, access_flags) {
                match decoded {
                    LoongArchVmExit::MmioRead {
                        addr,
                        width,
                        reg,
                        reg_width,
                        signed_ext,
                    } => {
                        return Ok(LoongArchExitKind::MmioRead {
                            addr: loong_guest_phys_addr_to_ax(addr),
                            width: loong_access_width_to_ax(width),
                            reg,
                            reg_width: loong_access_width_to_ax(reg_width),
                            signed_ext,
                            fallback: Some(fault_flags),
                        });
                    }
                    LoongArchVmExit::MmioWrite { addr, width, data } => {
                        return Ok(LoongArchExitKind::MmioWrite {
                            addr: loong_guest_phys_addr_to_ax(addr),
                            width: loong_access_width_to_ax(width),
                            data,
                            fallback: Some(fault_flags),
                        });
                    }
                    _ => {}
                }
            }
            Ok(LoongArchExitKind::NestedPageFault {
                addr: loong_guest_phys_addr_to_ax(addr),
                access_flags: fault_flags,
            })
        }
        LoongArchVmExit::Idle => Ok(LoongArchExitKind::Idle),
        LoongArchVmExit::Halt => Ok(LoongArchExitKind::Halt),
        LoongArchVmExit::Nothing => Ok(LoongArchExitKind::Nothing),
    }
}

/// Retires the guest PC for a device access that had no page-fault fallback.
fn retire_device_action(
    action: VcpuAction<LoongArchCompletion, GuestRequest>,
) -> VcpuAction<LoongArchCompletion, GuestRequest> {
    match action {
        VcpuAction::Reenter(completion) => VcpuAction::Reenter(LoongArchCompletion {
            advance_pc: true,
            ..completion
        }),
        other => other,
    }
}

fn publish_pch_pic_outputs(
    pch_pic: &Arc<dyn axdevice::PchPicOutputPort>,
    run: &irq::LoongArchRunPort,
) -> AxVmResult {
    while let Some(event) = pch_pic.take_output_event() {
        if !event.asserted {
            continue;
        }
        run.publish_external(0, event.vector).map_err(|error| {
            AxVmError::interrupt("publish LoongArch PCH-PIC output", format!("{error:?}"))
        })?;
    }
    Ok(())
}

struct AxvmLoongArchHostOps;

impl LoongArchHostOps for AxvmLoongArchHostOps {
    type TimerHandle = <crate::host::arceos::ArceOsHost as HostTimer>::TimerHandle;

    fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr {
        LoongArchHostPhysAddr::from_usize(
            default_host()
                .virt_to_phys(VirtAddr::from(vaddr.as_usize()))
                .as_usize(),
        )
    }

    fn current_time_nanos() -> u64 {
        default_host().monotonic_time().as_nanos() as u64
    }

    fn ticks_to_nanos(ticks: u64) -> u64 {
        ax_std::os::arceos::modules::ax_hal::time::ticks_to_nanos(ticks)
    }

    fn register_timer(
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> LoongArchVcpuResult<Self::TimerHandle> {
        default_host()
            .register_timer(deadline, callback)
            .map_err(|_| LoongArchVcpuError::TimerUnavailable)
    }

    fn cancel_timer(handle: Self::TimerHandle) -> LoongArchVcpuResult {
        default_host()
            .cancel_timer_and_wait(handle)
            .map(|_| ())
            .map_err(|_| LoongArchVcpuError::TimerUnavailable)
    }
}

pub(crate) struct AxvmLoongArchVcpu {
    backend: LoongArchVcpu<AxvmLoongArchHostOps>,
    vcpu_id: usize,
}

impl AxvmLoongArchVcpu {
    fn advance_guest_pc(&mut self) {
        self.backend.advance_guest_pc();
    }

    fn inject_eiointc_interrupt(&mut self, vector: usize) -> AxVmResult {
        loongarch_result(self.backend.inject_eiointc_interrupt(vector))
            .map_err(|error| AxVmError::interrupt("inject LoongArch EIOINTC interrupt", error))
    }

    fn inject_external_interrupt(&mut self, vector: usize, physical_irq: usize) -> AxVmResult {
        loongarch_result(self.backend.inject_external_interrupt(vector, physical_irq))
            .map_err(|error| AxVmError::interrupt("inject LoongArch external interrupt", error))
    }

    fn has_enabled_pending_interrupt(&self) -> bool {
        self.backend.has_enabled_pending_interrupt()
    }

    fn decode_mmio_fault(
        &mut self,
        addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    ) -> Option<LoongArchVmExit> {
        self.backend.decode_mmio_fault(addr, access_flags)
    }

    /// Resolves the pinned host CPU-local operands of one raw LVZ exit.
    ///
    /// Runs inside the pinned guest-entry scope, so it may read and write this
    /// CPU's CPUCFG and IOCSR banks directly. It performs only those bounded
    /// local accesses: no timer registration, allocation, device service or
    /// sleeping lock is touched while the backend is bound.
    fn capture_pinned_host(&self, exit: &ax_cpu::virtualization::Exit) -> LoongArchPinnedHost {
        self.backend.capture_pinned_host(exit)
    }
}

impl VcpuLocalInterrupts for AxvmLoongArchVcpu {
    type Snapshot = bool;
    type Completion = ();
    type Error = LoongArchVcpuError;

    fn prepare_entry(&mut self) -> Result<Self::Snapshot, Self::Error> {
        self.backend.prepare_entry();
        Ok(self.backend.has_enabled_pending_interrupt())
    }

    fn inject(&mut self, interrupt: PendingVcpuInterrupt) -> Result<(), Self::Error> {
        self.backend.inject_interrupt(interrupt.id.0 as usize)
    }

    fn handle_eoi(&mut self, token: DeliveryToken) -> Result<Self::Completion, Self::Error> {
        if token.target.vcpu_id != self.vcpu_id
            || token.sequence == 0
            || token.source.controller != axdevice_base::InterruptControllerId::new(0)
        {
            return Err(LoongArchVcpuError::InvalidInput);
        }
        // PCH-PIC/EIOINTC acknowledge state is owned by the shared endpoint;
        // CPUINTC has no separate guest EOI register in this backend.
        Ok(())
    }

    fn save_exit(&mut self) -> Result<Self::Completion, Self::Error> {
        Ok(())
    }

    fn reset(&mut self) {
        let _ = self.backend.quiet_timer();
    }
}

impl VcpuLocalTimer for AxvmLoongArchVcpu {
    type Error = LoongArchVcpuError;

    fn arm(&mut self, deadline: u64) -> Result<(), Self::Error> {
        self.backend.arm_timer(deadline)
    }

    fn suspend(&mut self) -> Result<(), Self::Error> {
        self.backend.suspend_timer()
    }

    fn resume(&mut self) -> Result<(), Self::Error> {
        self.backend.resume_timer()
    }

    fn cancel(&mut self) -> Result<(), Self::Error> {
        self.backend.quiet_timer()
    }

    fn consume_expiry(&mut self) -> bool {
        self.backend.consume_timer_expiry()
    }
}

impl VmArchVcpuOps for AxvmLoongArchVcpu {
    type CreateConfig = LoongArchVCpuCreateConfig;
    type SetupConfig = LoongArchVCpuSetupConfig;
    type Exit = LoongArchVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        loongarch_result(LoongArchVcpu::new(vm_id, vcpu_id, config))
            .map(|backend| Self { backend, vcpu_id })
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        loongarch_result(self.backend.set_entry(ax_guest_phys_addr_to_loong(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        loongarch_result(
            self.backend
                .set_nested_page_table(ax_nested_paging_to_loong(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        loongarch_result(self.backend.setup(config))
    }

    fn run(&mut self) -> BackendResult<Self::Exit> {
        loongarch_result(self.backend.run_machine())
    }

    fn bind(&mut self) -> BackendResult {
        loongarch_result(self.backend.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        loongarch_result(self.backend.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.backend.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        loongarch_result(self.backend.inject_interrupt(vector))
    }

    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> BackendResult {
        // The PCH-PIC/EIOINTC Router consumes line trigger semantics before
        // emitting a guest vector. The vCPU injection is mode-agnostic.
        match trigger {
            InterruptTriggerMode::EdgeTriggered | InterruptTriggerMode::LevelTriggered => {
                loongarch_result(self.backend.inject_interrupt(vector))
            }
        }
    }

    fn set_return_value(&mut self, val: usize) {
        self.backend.set_return_value(val);
    }
}

pub(crate) struct AxvmLoongArchPerCpu(ax_cpu::virtualization::PerCpu);

impl VmArchPerCpuOps for AxvmLoongArchPerCpu {
    fn new(_cpu_id: usize) -> BackendResult<Self> {
        Ok(Self(ax_cpu::virtualization::PerCpu::new()))
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> BackendResult {
        // SAFETY: AxVM owns this CPU before guest scheduling begins.
        unsafe { self.0.enable() }.map_err(|_| BackendError::InvalidState)
    }

    fn hardware_disable(&mut self) -> BackendResult {
        // SAFETY: AxVM stops and unbinds guests before releasing this CPU.
        unsafe { self.0.disable() }.map_err(|_| BackendError::InvalidState)
    }

    fn max_guest_page_table_levels(&self) -> usize {
        4
    }
}

fn loongarch_result<T>(result: LoongArchVcpuResult<T>) -> BackendResult<T> {
    result.map_err(loongarch_error_to_backend)
}

fn loongarch_error_to_backend(err: LoongArchVcpuError) -> BackendError {
    match err {
        LoongArchVcpuError::InvalidInput => BackendError::InvalidInput,
        LoongArchVcpuError::Unsupported => BackendError::Unsupported,
        LoongArchVcpuError::BadState => BackendError::InvalidState,
        LoongArchVcpuError::TimerUnavailable => BackendError::InvalidState,
    }
}

fn ax_guest_phys_addr_to_loong(addr: GuestPhysAddr) -> LoongArchGuestPhysAddr {
    LoongArchGuestPhysAddr::from_usize(addr.as_usize())
}

fn loong_guest_phys_addr_to_ax(addr: LoongArchGuestPhysAddr) -> GuestPhysAddr {
    GuestPhysAddr::from(addr.as_usize())
}

fn ax_nested_paging_to_loong(config: NestedPagingConfig) -> LoongArchNestedPagingConfig {
    LoongArchNestedPagingConfig::new(
        config.root_paddr.as_usize(),
        config.levels,
        config.gpa_bits,
        config.mode,
    )
}

fn loong_access_width_to_ax(width: LoongArchAccessWidth) -> AccessWidth {
    match width {
        LoongArchAccessWidth::Byte => AccessWidth::Byte,
        LoongArchAccessWidth::Word => AccessWidth::Word,
        LoongArchAccessWidth::Dword => AccessWidth::Dword,
        LoongArchAccessWidth::Qword => AccessWidth::Qword,
    }
}

fn loong_access_flags_to_ax(flags: LoongArchAccessFlags) -> MappingFlags {
    let mut converted = MappingFlags::empty();
    if flags.contains(LoongArchAccessFlags::READ) {
        converted |= MappingFlags::READ;
    }
    if flags.contains(LoongArchAccessFlags::WRITE) {
        converted |= MappingFlags::WRITE;
    }
    if flags.contains(LoongArchAccessFlags::EXECUTE) {
        converted |= MappingFlags::EXECUTE;
    }
    if flags.contains(LoongArchAccessFlags::USER) {
        converted |= MappingFlags::USER;
    }
    if flags.contains(LoongArchAccessFlags::DEVICE) {
        converted |= MappingFlags::DEVICE;
    }
    if flags.contains(LoongArchAccessFlags::UNCACHED) {
        converted |= MappingFlags::UNCACHED;
    }
    converted
}

pub(super) fn make_guest_memory_visible(addr: VirtAddr, size: usize) {
    // SAFETY: the VM memory owner retains the mapped image buffer and serializes
    // publication to guests until the writeback and completion barrier finish.
    let range = ax_cpu::cache::CacheRange::new(addr, size).expect("mapped guest image range");
    unsafe { ax_cpu::cache::clean_invalidate_dcache_range(range) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_loongarch_vcpu_errors_to_backend_errors() {
        assert_eq!(
            loongarch_error_to_backend(LoongArchVcpuError::InvalidInput),
            BackendError::InvalidInput
        );
        assert_eq!(
            loongarch_error_to_backend(LoongArchVcpuError::Unsupported),
            BackendError::Unsupported
        );
        assert_eq!(
            loongarch_error_to_backend(LoongArchVcpuError::BadState),
            BackendError::InvalidState
        );
    }

    #[test]
    fn converts_loongarch_value_types_to_axvm_value_types() {
        assert_eq!(
            loong_guest_phys_addr_to_ax(LoongArchGuestPhysAddr::from_usize(0x4000)).as_usize(),
            0x4000
        );
        assert_eq!(
            loong_access_width_to_ax(LoongArchAccessWidth::Dword),
            AccessWidth::Dword
        );
        assert_eq!(
            loong_access_flags_to_ax(LoongArchAccessFlags::READ | LoongArchAccessFlags::WRITE),
            MappingFlags::READ | MappingFlags::WRITE
        );
    }
}
