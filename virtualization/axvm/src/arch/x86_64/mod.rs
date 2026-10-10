//! AxVM x86_64 adapter.
//!
//! CPU mechanisms come from ax-cpu; this module owns VM policy, the ArceOS
//! integration, and the x86_vlapic device model.

use std::{
    arch::asm,
    collections::BTreeMap,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, Weak,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use ax_std::os::arceos::{
    guard::IrqSaveGuard,
    sync::{RawSpinLock, RawSpinLockIrqSaveGuard},
};
use axdevice::*;
use axdevice_base::*;
use axvm_types::{VmBackendError as BackendError, VmBackendResult as BackendResult, *};
use x86_vlapic::*;

use crate::arch::x86_64::policy::{
    X86AccessWidth, X86GuestPhysAddr, X86HostPhysAddr, X86HostVirtAddr, X86MsrAddr, X86Port, *,
};

mod control_memory;
pub(crate) mod policy;

use super::*;
use crate::{
    architecture::{HypercallExit, MmioReadExit, MmioWriteExit, ops::RegisterCompletion},
    engine::{VcpuAction, WaitReason},
    host::*,
    irq::model::{
        InterruptControllerEndpoint, InterruptControllerOwner, InterruptSourceId,
        PendingVcpuInterrupt, SourceEvent, VcpuLocalInterrupts, VcpuLocalTimer, VirtualInterruptId,
    },
    runtime::{QueuedVcpuInterrupt, hvc::GuestRequest},
    services::{RunServices, RunSignals, SignalError, VcpuWait},
    sync::MutexExt,
};

mod acpi_pm_timer;
pub(crate) mod boot;
mod capabilities;
mod cmos;
mod exit;
mod host_irq;
pub(crate) mod irq;
mod nested_paging;
mod pci_config;
mod pic;
pub(crate) mod port;
mod resource_pools;
mod runtime_port;
mod vm;
use exit::*;
use runtime_port::{AxvmX86VlapicRuntime, X86RunBinding, X86TimerHandle};
pub(crate) use vm::X86VmPlan;

use crate::architecture::sysreg::{self, SysRegWriteExit};

const RFLAGS_INTERRUPT_FLAG: u64 = 1 << 9;

pub(crate) struct X86_64Arch;

/// Owned x86 VM exit produced after the backend binding is released.
///
/// Every value needed by the task-context interpreter is captured while the
/// backend is still loaded; the interpreter itself never touches a hardware
/// register.
#[derive(Debug)]
pub(crate) enum X86Exit {
    Hypercall {
        nr: u64,
        args: [u64; 6],
    },
    PortIoRead {
        exit: IoReadExit,
        next_rip: u64,
    },
    PortIoWrite {
        exit: IoWriteExit,
        next_rip: u64,
    },
    PortIoString(X86PortIoStringExit),
    MmioRead {
        exit: MmioReadExit,
        next_rip: u64,
    },
    MmioReadByte {
        addr: GuestPhysAddr,
        width: AccessWidth,
        reg: X86ByteRegister,
        next_rip: u64,
    },
    MmioReadWord {
        addr: GuestPhysAddr,
        width: AccessWidth,
        reg: usize,
        next_rip: u64,
    },
    MmioReadRsp {
        addr: GuestPhysAddr,
        width: AccessWidth,
        rsp_width: X86AccessWidth,
        next_rip: u64,
    },
    MmioWrite {
        exit: MmioWriteExit,
        next_rip: u64,
    },
    MsrRead {
        addr: SysRegAddr,
        next_rip: u64,
    },
    MsrWrite {
        exit: SysRegWriteExit,
        next_rip: u64,
    },
    NestedPageFault(NestedPageFaultExit),
    PreemptionTimer,
    InterruptEnd(Option<u8>),
    Halt,
    SystemDown,
    FailEntry(usize),
    Nothing,
}

impl X86Exit {
    fn capture(exit: <AxvmX86Vcpu as VmArchVcpuOps>::Exit) -> Self {
        match exit {
            X86VmExit::Hypercall { nr, args } => Self::Hypercall { nr, args },
            X86VmExit::PortIoRead {
                port,
                width,
                next_rip,
            } => Self::PortIoRead {
                exit: IoReadExit {
                    port: x86_port_to_ax(port),
                    width: x86_access_width_to_ax(width),
                },
                next_rip,
            },
            X86VmExit::PortIoWrite {
                port,
                width,
                data,
                next_rip,
            } => Self::PortIoWrite {
                exit: IoWriteExit {
                    port: x86_port_to_ax(port),
                    width: x86_access_width_to_ax(width),
                    data,
                },
                next_rip,
            },
            X86VmExit::PortIoString(exit) => Self::PortIoString(exit),
            X86VmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
                byte_reg,
                next_rip,
            } => {
                let addr = x86_guest_phys_addr_to_ax(addr);
                let operand_width = width;
                let width = x86_access_width_to_ax(width);
                match byte_reg {
                    Some(reg) => Self::MmioReadByte {
                        addr,
                        width,
                        reg,
                        next_rip,
                    },
                    None if reg == 4 => Self::MmioReadRsp {
                        addr,
                        width,
                        rsp_width: operand_width,
                        next_rip,
                    },
                    None if width == AccessWidth::Word => Self::MmioReadWord {
                        addr,
                        width,
                        reg,
                        next_rip,
                    },
                    None => Self::MmioRead {
                        exit: MmioReadExit {
                            addr,
                            width,
                            reg,
                            reg_width: x86_access_width_to_ax(reg_width),
                            signed_ext,
                        },
                        next_rip,
                    },
                }
            }
            X86VmExit::MmioWrite {
                addr,
                width,
                data,
                next_rip,
            } => Self::MmioWrite {
                exit: MmioWriteExit {
                    addr: x86_guest_phys_addr_to_ax(addr),
                    width: x86_access_width_to_ax(width),
                    data,
                },
                next_rip,
            },
            X86VmExit::MsrRead { addr, next_rip } => Self::MsrRead {
                addr: x86_msr_addr_to_ax(addr),
                next_rip,
            },
            X86VmExit::MsrWrite {
                addr,
                value,
                next_rip,
            } => Self::MsrWrite {
                exit: SysRegWriteExit {
                    addr: x86_msr_addr_to_ax(addr),
                    value,
                },
                next_rip,
            },
            X86VmExit::NestedPageFault { addr, access_flags } => {
                Self::NestedPageFault(NestedPageFaultExit {
                    addr: x86_guest_phys_addr_to_ax(addr),
                    access_flags: x86_access_flags_to_ax(access_flags),
                })
            }
            X86VmExit::PreemptionTimer => Self::PreemptionTimer,
            X86VmExit::InterruptEnd { vector } => Self::InterruptEnd(vector),
            X86VmExit::Halt => Self::Halt,
            X86VmExit::SystemDown => Self::SystemDown,
            X86VmExit::FailEntry {
                hardware_entry_failure_reason,
            } => Self::FailEntry(hardware_entry_failure_reason),
            X86VmExit::Nothing => Self::Nothing,
        }
    }

    /// Interprets one owned x86 exit in task context with sleepable services.
    fn handle(
        self,
        vcpu_id: usize,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<X86Completion, GuestRequest>> {
        match self {
            Self::Hypercall { nr, args } => {
                crate::architecture::exit::handle_hypercall::<X86_64Arch>(
                    services,
                    vcpu_id,
                    HypercallExit { nr, args },
                    crate::runtime::hvc::HyperCallAbi::native(),
                )
            }
            Self::PortIoRead { exit, next_rip } => Ok(retire_action(
                exit::handle_io_read(services, vcpu_id, exit)?,
                next_rip,
            )),
            Self::PortIoWrite { exit, next_rip } => Ok(retire_action(
                exit::handle_io_write(services, vcpu_id, exit)?,
                next_rip,
            )),
            Self::PortIoString(exit) => exit::handle_io_string(services, vcpu_id, exit),
            Self::MmioRead { exit, next_rip } => Ok(retire_action(
                crate::architecture::exit::handle_mmio_read::<X86_64Arch>(services, vcpu_id, exit)?,
                next_rip,
            )),
            Self::MmioWrite { exit, next_rip } => Ok(retire_action(
                crate::architecture::exit::handle_mmio_write::<X86_64Arch>(
                    services, vcpu_id, exit,
                )?,
                next_rip,
            )),
            Self::MmioReadByte {
                addr,
                width,
                reg,
                next_rip,
            } => {
                let raw =
                    crate::architecture::exit::read_mmio_value(services, vcpu_id, addr, width)?;
                let value = (raw & crate::vm::width_mask(width)) as u8;
                Ok(VcpuAction::Reenter(
                    X86Completion::ByteGpr {
                        register: reg,
                        value,
                    }
                    .retire(next_rip),
                ))
            }
            Self::MmioReadWord {
                addr,
                width,
                reg,
                next_rip,
            } => {
                let raw =
                    crate::architecture::exit::read_mmio_value(services, vcpu_id, addr, width)?;
                let value = (raw & crate::vm::width_mask(width)) as u16;
                Ok(VcpuAction::Reenter(
                    X86Completion::WordGpr {
                        register: reg,
                        value,
                    }
                    .retire(next_rip),
                ))
            }
            Self::MmioReadRsp {
                addr,
                width,
                rsp_width,
                next_rip,
            } => {
                let raw =
                    crate::architecture::exit::read_mmio_value(services, vcpu_id, addr, width)?;
                let value = raw & crate::vm::width_mask(width);
                Ok(VcpuAction::Reenter(
                    X86Completion::Rsp {
                        width: rsp_width,
                        value: value as u64,
                    }
                    .retire(next_rip),
                ))
            }
            Self::MsrRead { addr, next_rip } => {
                // `RDMSR` must land in `EDX:EAX`, so this reads the raw 64-bit
                // value from the device service and builds the x86-specific
                // completion instead of the generic single-register one.
                let value = read_msr_value(services, vcpu_id, addr)?;
                Ok(VcpuAction::Reenter(
                    X86Completion::MsrRead { value }.retire(next_rip),
                ))
            }
            Self::MsrWrite { exit, next_rip } => Ok(retire_action(
                sysreg::handle_write::<X86_64Arch>(services, vcpu_id, exit)?,
                next_rip,
            )),
            Self::NestedPageFault(exit) => Ok(VcpuAction::Control(GuestRequest::NestedFault {
                addr: exit.addr,
                access_flags: exit.access_flags,
            })),
            Self::PreemptionTimer => Ok(VcpuAction::Reenter(X86Completion::default())),
            Self::InterruptEnd(vector) => {
                if let Some(vector) = vector {
                    irq::inject_pending_ioapic_irq_after_eoi(services, vcpu_id, vector);
                }
                Ok(VcpuAction::Reenter(X86Completion::default()))
            }
            Self::Halt => Ok(VcpuAction::Wait(WaitReason { return_value: None })),
            Self::SystemDown => Ok(VcpuAction::Stop(StopReason::SystemDown)),
            Self::FailEntry(reason) => {
                warn!("x86 vCPU[{vcpu_id}] guest entry failed: {reason:#x}");
                Ok(VcpuAction::Reenter(X86Completion::default()))
            }
            Self::Nothing => Ok(VcpuAction::Reenter(X86Completion::default())),
        }
    }
}

/// Attaches the decoded retirement `RIP` to a device-serviced reentry.
///
/// The x86 device handlers always answer `Reenter`; the RIP is installed by
/// [`X86Completion::commit`] on the next bound entry, never while the device
/// access is unresolved.
fn retire_action(
    action: VcpuAction<X86Completion, GuestRequest>,
    next_rip: u64,
) -> VcpuAction<X86Completion, GuestRequest> {
    match action {
        VcpuAction::Reenter(completion) => VcpuAction::Reenter(completion.retire(next_rip)),
        other => other,
    }
}

/// Reads one guest MSR through the run-bound device service in task context.
///
/// The shared system-register path returns a single general-purpose register,
/// but `RDMSR` needs the full 64-bit value so the x86 completion can split it
/// into `EDX:EAX`.
fn read_msr_value(services: &RunServices, vcpu_id: usize, addr: SysRegAddr) -> AxVmResult<u64> {
    services
        .read_device(&DeviceAccess::new(
            DeviceVcpuId::new(vcpu_id),
            BusKind::SysReg,
            addr.addr() as u64,
            AccessWidth::Qword,
        ))?
        .ok_or_else(|| missing_msr_error("read", addr))
}

fn missing_msr_error(operation: &'static str, addr: SysRegAddr) -> crate::AxVmError {
    crate::AxVmError::device(
        "access guest system register",
        axdevice::DeviceManagerError::Access {
            operation,
            bus: BusKind::SysReg,
            addr: addr.addr() as u64,
            width: AccessWidth::Qword,
            source: axdevice_base::DeviceError::NotFound,
        },
    )
}

/// Owned register/port effect committed on the next bound guest entry.
pub(crate) enum X86Completion {
    Register(RegisterCompletion),
    /// A device-serviced access whose guest `RIP` must be retired only after the
    /// device service succeeded. `inner` carries the register/port effect that
    /// is committed first, then the decoded retirement `RIP` is installed.
    Retire {
        next_rip: u64,
        inner: Box<X86Completion>,
    },
    ByteGpr {
        register: X86ByteRegister,
        value: u8,
    },
    WordGpr {
        register: usize,
        value: u16,
    },
    Rsp {
        width: X86AccessWidth,
        value: u64,
    },
    /// A device-serviced MSR read committed on the next bound guest entry.
    ///
    /// `RDMSR` returns the 64-bit value in `EDX:EAX`, so the low half is
    /// zero-extended into `EAX` and the high half into `EDX`. Neither register
    /// may keep stale upper bits, which a single 64-bit `RAX` write cannot
    /// express.
    MsrRead {
        value: u64,
    },
    PortIoString(X86PortIoStringExit),
}

impl Default for X86Completion {
    fn default() -> Self {
        Self::Register(RegisterCompletion::None)
    }
}

impl From<RegisterCompletion> for X86Completion {
    fn from(completion: RegisterCompletion) -> Self {
        Self::Register(completion)
    }
}

impl X86Completion {
    /// Wraps `self` so its retirement `RIP` is installed after it commits.
    fn retire(self, next_rip: u64) -> Self {
        Self::Retire {
            next_rip,
            inner: Box::new(self),
        }
    }

    fn commit(self, vcpu: &mut AxvmX86Vcpu) -> AxVmResult {
        match self {
            Self::Register(RegisterCompletion::None) => Ok(()),
            Self::Register(RegisterCompletion::Gpr { register, value }) => {
                vcpu.set_gpr(register, value);
                Ok(())
            }
            Self::Register(RegisterCompletion::Return(value)) => {
                vcpu.set_return_value(value);
                Ok(())
            }
            Self::Retire { next_rip, inner } => {
                inner.commit(vcpu)?;
                vcpu.set_rip(next_rip).map_err(|error| {
                    crate::vcpu::map_vcpu_backend_error("retire x86 guest RIP", error)
                })
            }
            Self::ByteGpr { register, value } => {
                vcpu.set_gpr_byte(register, value);
                Ok(())
            }
            Self::WordGpr { register, value } => {
                vcpu.set_gpr_word(register, value);
                Ok(())
            }
            Self::Rsp { width, value } => {
                vcpu.set_gpr_rsp(width, value);
                Ok(())
            }
            Self::MsrRead { value } => {
                vcpu.set_gpr(0, (value & 0xffff_ffff) as usize);
                vcpu.set_gpr(2, (value >> 32) as usize);
                Ok(())
            }
            Self::PortIoString(exit) => vcpu.complete_port_io_string(exit).map_err(|error| {
                crate::vcpu::map_vcpu_backend_error("complete x86 string I/O", error)
            }),
        }
    }
}

/// Run-bound x86 interrupt capabilities retained for one execution period.
///
/// The entry keeps only the lower [`X86DeliveryPort`]; per-entry injection and
/// the task-side registration maps stay in the device runtime, so the entry
/// never reaches the VM registry or a sleeping lock.
pub(crate) struct X86Entry {
    port: Option<Arc<X86DeliveryPort>>,
}

impl X86Entry {
    fn prepare(resources: &crate::vm::AxVMResources, signals: Arc<RunSignals>) -> AxVmResult<Self> {
        let devices = resources.devices()?;
        let services = devices.services();
        let port = services
            .require::<X86InterruptDomainRuntimeKey>()
            .ok()
            .map(|domain| domain.delivery_port());
        if let Some(port) = &port {
            let pic = services.require::<X86PicServiceKey>().ok();
            let ioapic = services.require::<X86InterruptDomainKey>().ok();
            port.run_binding().bind(signals, pic, ioapic);
        }
        Ok(Self { port })
    }

    fn prepare_vcpu(&self, _vcpu: &mut AxvmX86Vcpu) -> AxVmResult {
        Ok(())
    }

    fn before_guest(&self, vcpu_id: usize, vcpu: &mut AxvmX86Vcpu) -> AxVmResult {
        irq::drain_pending_wired_irqs(self.port.as_deref(), vcpu_id, vcpu);
        irq::drain_pending_ioapic_irqs(self.port.as_deref(), vcpu_id, vcpu);
        irq::activate_ready_ioapic_forwarding_routes(self.port.as_deref());
        Ok(())
    }

    fn invalidate_translations(&self, old_root: NestedPagingConfig) -> AxVmResult {
        nested_paging::invalidate_translations(old_root)
    }
}

impl ArchOps for X86_64Arch {
    type VCpu = AxvmX86Vcpu;
    type PerCpu = AxvmX86PerCpu;
    type NestedPageTable = nested_paging::NestedPageTable<crate::HostPagingHandler>;
    type Entry = X86Entry;
    type Exit = X86Exit;
    type Completion = X86Completion;

    fn has_hardware_support() -> bool {
        crate::arch::x86_64::policy::initialize_hardware_support().is_ok()
    }

    fn invalidate_translations(entry: &Self::Entry, old_root: NestedPagingConfig) -> AxVmResult {
        entry.invalidate_translations(old_root)
    }

    fn prepare_entry(
        resources: &crate::vm::AxVMResources,
        signals: Arc<RunSignals>,
    ) -> AxVmResult<Self::Entry> {
        X86Entry::prepare(resources, signals)
    }

    fn enter_runtime(vm: &mut crate::AxVM, signals: &Arc<RunSignals>) -> AxVmResult {
        irq::enter_runtime(vm, signals)
    }

    fn exit_runtime(vm: &mut crate::AxVM, _signals: &Arc<RunSignals>) -> AxVmResult {
        irq::exit_runtime(vm)
    }

    fn prepare_vcpu(vcpu: &mut Self::VCpu, entry: &Self::Entry) -> AxVmResult {
        entry.prepare_vcpu(vcpu)
    }

    /// Quiesces the per-vCPU LAPIC timer before a task-side pause is ACKed.
    fn suspend_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        vcpu.suspend_timer()
            .map_err(|error| crate::vcpu::map_vcpu_backend_error("suspend x86 vCPU timer", error))
    }

    /// Reinstalls the per-vCPU LAPIC timer before reopening the guest entry.
    fn resume_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        vcpu.resume_timer()
            .map_err(|error| crate::vcpu::map_vcpu_backend_error("resume x86 vCPU timer", error))
    }

    /// Stops the per-vCPU LAPIC producer before a backend is released or reaped.
    fn quiet_vcpu(vcpu: &mut Self::VCpu) -> AxVmResult {
        vcpu.stop_timer()
            .map_err(|error| crate::vcpu::map_vcpu_backend_error("quiesce x86 vCPU timer", error))
    }

    fn before_guest(vcpu: &mut Self::VCpu, vcpu_id: usize, entry: &Self::Entry) -> AxVmResult {
        entry.before_guest(vcpu_id, vcpu)
    }

    fn complete(
        vcpu: &mut Self::VCpu,
        _entry: &Self::Entry,
        completion: Self::Completion,
    ) -> AxVmResult {
        completion.commit(vcpu)
    }

    fn capture_exit(
        _vcpu: &mut Self::VCpu,
        _entry: &Self::Entry,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<Self::Exit> {
        Ok(X86Exit::capture(exit))
    }

    fn handle_exit(
        exit: Self::Exit,
        vcpu_id: usize,
        services: &RunServices,
    ) -> AxVmResult<VcpuAction<Self::Completion, GuestRequest>> {
        exit.handle(vcpu_id, services)
    }

    fn inject_arch_interrupt(
        vcpu: &mut Self::VCpu,
        _vcpu_id: usize,
        _entry: &Self::Entry,
        interrupt: crate::runtime::QueuedVcpuInterrupt,
    ) -> AxVmResult {
        let QueuedVcpuInterrupt::LegacyPic { vector } = interrupt else {
            unreachable!("x86 architecture interrupt sources are legacy PIC ExtINT vectors");
        };
        vcpu.inject_legacy_pic_interrupt(vector).map_err(|error| {
            crate::vcpu::map_vcpu_backend_error("inject x86 legacy PIC interrupt", error)
        })
    }

    fn wait_for_event(
        vcpu: &mut Self::VCpu,
        _vcpu_id: usize,
        _entry: &Self::Entry,
        wait: &VcpuWait,
    ) -> AxVmResult {
        // `wait_until` takes an `Fn` predicate, so the backend is borrowed once
        // here and the closure only performs this vCPU's own lower-state read.
        // It never mutates the backend, queries the VM, or takes a sleep lock.
        let backend: &AxvmX86Vcpu = vcpu;
        wait.wait_until(|| backend.has_pending_event());
        Ok(())
    }
}

pub(crate) struct AxvmX86HostOps;

impl X86VlapicHostOps for AxvmX86HostOps {
    type TimerHandle = X86TimerHandle;
    type Runtime = AxvmX86VlapicRuntime;

    fn alloc_frame() -> Option<x86_vlapic::X86HostPhysAddr> {
        default_host()
            .alloc_frame()
            .map(|addr| x86_vlapic::X86HostPhysAddr::from_usize(addr.as_usize()))
    }

    fn dealloc_frame(paddr: x86_vlapic::X86HostPhysAddr) {
        default_host().dealloc_frame(axvm_types::HostPhysAddr::from(paddr.as_usize()));
    }

    fn phys_to_virt(paddr: x86_vlapic::X86HostPhysAddr) -> x86_vlapic::X86HostVirtAddr {
        let vaddr = default_host().phys_to_virt(axvm_types::HostPhysAddr::from(paddr.as_usize()));
        x86_vlapic::X86HostVirtAddr::from_usize(vaddr.as_usize())
    }

    fn virt_to_phys(vaddr: x86_vlapic::X86HostVirtAddr) -> x86_vlapic::X86HostPhysAddr {
        let paddr = default_host().virt_to_phys(axvm_types::HostVirtAddr::from(vaddr.as_usize()));
        x86_vlapic::X86HostPhysAddr::from_usize(paddr.as_usize())
    }

    fn current_time_nanos() -> u64 {
        ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos()
    }

    fn unbound_runtime(vm_id: X86VmId, vcpu_id: X86VcpuId) -> Self::Runtime {
        // Host-side adapters that are not attached to a guest run receive an
        // inactive port. Real vCPU-owned vLAPICs are created by the run owner
        // with the run-shared binding instead of this constructor.
        AxvmX86VlapicRuntime::new(vm_id, vcpu_id, Arc::new(X86RunBinding::new()))
    }
}

#[cfg(test)]
fn route_pit_irq(
    pulse_pic: impl FnOnce() -> Option<u8>,
    assert_ioapic: impl FnOnce() -> Option<IoApicInterrupt>,
    inject: impl FnMut(u8, InterruptTriggerMode, PitInterruptSource) -> X86VlapicResult,
) -> X86VlapicResult {
    route_pit_claim(
        pulse_pic,
        |vector| *vector,
        |_vector| {},
        assert_ioapic,
        inject,
    )
}

#[cfg(test)]
fn route_pit_claim<C>(
    claim_pic: impl FnOnce() -> Option<C>,
    pic_vector: impl FnOnce(&C) -> u8,
    restore_pic: impl FnOnce(C),
    assert_ioapic: impl FnOnce() -> Option<IoApicInterrupt>,
    inject: impl FnMut(u8, InterruptTriggerMode, PitInterruptSource) -> X86VlapicResult,
) -> X86VlapicResult {
    route_pit_claims(
        claim_pic,
        pic_vector,
        restore_pic,
        [assert_ioapic()],
        inject,
    )
}

#[cfg(test)]
fn route_pit_claims<C>(
    claim_pic: impl FnOnce() -> Option<C>,
    pic_vector: impl FnOnce(&C) -> u8,
    restore_pic: impl FnOnce(C),
    ioapic_interrupts: impl IntoIterator<Item = Option<IoApicInterrupt>>,
    mut inject: impl FnMut(u8, InterruptTriggerMode, PitInterruptSource) -> X86VlapicResult,
) -> X86VlapicResult {
    // KVM fans GSI 0 out to both in-kernel irqchips. Each controller owns its
    // mask/in-service state and independently decides whether this edge is
    // currently deliverable. The second IOAPIC input preserves the standard
    // MPS IRQ0 -> INTIN2 route while the first preserves the ACPI GSI0 route.
    let pic_claim = claim_pic();
    let mut first_error = None;

    if let Some(claim) = pic_claim {
        let vector = pic_vector(&claim);
        if let Err(error) = inject(
            vector,
            InterruptTriggerMode::EdgeTriggered,
            PitInterruptSource::LegacyPic,
        ) {
            restore_pic(claim);
            first_error = Some(error);
        }
    }
    for interrupt in ioapic_interrupts.into_iter().flatten() {
        let trigger = if interrupt.level_triggered {
            InterruptTriggerMode::LevelTriggered
        } else {
            InterruptTriggerMode::EdgeTriggered
        };
        if let Err(error) = inject(interrupt.vector, trigger, PitInterruptSource::IoApic)
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }

    first_error.map_or(Ok(()), Err)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(test)]
enum PitInterruptSource {
    LegacyPic,
    IoApic,
}

impl X86HostOps for AxvmX86HostOps {
    fn nanos_to_ticks(nanos: u64) -> u64 {
        ax_std::os::arceos::modules::ax_hal::time::nanos_to_ticks(nanos)
    }

    fn service_pending_host_interrupt() {
        let host_rflags = current_rflags();
        unsafe {
            asm!("sti", "nop", options(nomem, nostack));
        }
        restore_host_interrupt_flag(host_rflags);
    }

    fn dispatch_acknowledged_host_interrupt(vector: u8) {
        crate::host::arceos::dispatch_host_irq(vector as usize);
    }
}

/// AxVM-owned x86 vCPU backend.
///
/// The wrapper owns exactly one VMX or SVM backend and the vCPU's run-bound
/// interrupt port. Durable string-I/O progress is carried by an owned
/// [`X86Completion`] rather than stored on the backend.
pub(crate) struct AxvmX86Vcpu(X86Vcpu<AxvmX86HostOps, control_memory::ControlPages>);

// SAFETY: this private adapter transfers exclusively owned, stable ControlPages
// between the VM control task and one vCPU owner. Every CPU-local VMCS/VMCB
// binding is enclosed by AxVCpu::with_backend_bound_current_cpu, which unloads
// before releasing its pin and aborts if retirement fails. No bound adapter is
// published or transferred; reap_participants joins the old owner before reuse.
// Software registers and xstate/control leases are owned values; shared IRQ and
// timer ports contain only synchronized run state. This does not make the
// underlying CPU-bound Vmcs or this mutable adapter shareable through Sync.
unsafe impl Send for AxvmX86Vcpu {}

impl AxvmX86Vcpu {
    fn has_pending_event(&self) -> bool {
        self.0.has_pending_event()
    }

    /// Commits one string-I/O element after the task layer completed its
    /// memory and device access.
    fn complete_port_io_string(&mut self, exit: X86PortIoStringExit) -> BackendResult {
        x86_result(self.0.complete_port_io_string(exit))
    }

    /// Installs the guest `RIP` a device-serviced access retires.
    fn set_rip(&mut self, rip: u64) -> BackendResult {
        x86_result(self.0.set_rip(rip))
    }

    pub(crate) fn inject_legacy_pic_interrupt(&mut self, vector: u8) -> BackendResult {
        x86_result(self.0.inject_legacy_pic_interrupt(vector))
    }

    /// Quiesces this vCPU's local-APIC timer for a task-side VM suspend.
    fn suspend_timer(&mut self) -> BackendResult {
        x86_result(VcpuLocalTimer::suspend(&mut self.0))
    }

    /// Reinstalls this vCPU's local-APIC timer after a suspend.
    fn resume_timer(&mut self) -> BackendResult {
        x86_result(VcpuLocalTimer::resume(&mut self.0))
    }

    /// Cancels this vCPU's local-APIC timer and retires its state.
    fn stop_timer(&mut self) -> BackendResult {
        x86_result(VcpuLocalTimer::cancel(&mut self.0))
    }

    fn set_gpr_byte(&mut self, reg: X86ByteRegister, value: u8) {
        self.0.set_gpr_byte(reg, value);
    }

    fn set_gpr_word(&mut self, reg: usize, value: u16) {
        self.0.set_gpr_word(reg, value);
    }

    fn set_gpr_rsp(&mut self, width: X86AccessWidth, value: u64) {
        self.0.set_gpr_rsp(width, value);
    }
}

impl VmArchVcpuOps for AxvmX86Vcpu {
    type CreateConfig = X86VcpuCreateConfig<AxvmX86VlapicRuntime>;
    type SetupConfig = X86VcpuSetupConfig;
    type Exit = X86VmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        use ax_cpu::virtualization::{SvmControlMemory, VcpuControlMemory, VmxControlMemory};
        use control_memory::ControlPages;
        let memory = match x86_result(crate::arch::x86_64::policy::selected_nested_paging_format())?
        {
            X86NestedPagingFormat::Ept => VcpuControlMemory::Vmx(VmxControlMemory {
                vmcs: ControlPages::allocate(1)?,
                io_bitmap_a: ControlPages::allocate(1)?,
                io_bitmap_b: ControlPages::allocate(1)?,
                msr_bitmap: ControlPages::allocate(1)?,
            }),
            X86NestedPagingFormat::Npt => VcpuControlMemory::Svm(SvmControlMemory {
                guest: ControlPages::allocate(1)?,
                host: ControlPages::allocate(1)?,
                io_permissions: ControlPages::allocate(3)?,
                msr_permissions: ControlPages::allocate(2)?,
            }),
        };
        // SAFETY: VM construction executes at ring 0 on an initialized CPU.
        // The CPU backend revalidates this layout on every pinned binding.
        let layout = unsafe { ax_cpu::virtualization::XstateLayout::current() };
        let pages = layout.byte_len().div_ceil(4096);
        let xstate = ax_cpu::virtualization::GuestXstate::new(
            layout,
            ControlPages::allocate(pages)?,
            ControlPages::allocate(pages)?,
        )
        .map_err(|_| VmBackendError::InvalidInput)?;
        x86_result(X86Vcpu::new_with_config(
            vm_id, vcpu_id, config, memory, xstate,
        ))
        .map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        x86_result(self.0.set_entry(ax_guest_phys_addr_to_x86(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        x86_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_x86(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        x86_result(self.0.setup(config))
    }

    fn run(&mut self) -> BackendResult<Self::Exit> {
        let _entry_irq_guard = IrqSaveGuard::new();
        x86_result(VcpuLocalInterrupts::prepare_entry(&mut self.0))?;
        let exit = x86_result(self.0.run())?;
        x86_result(VcpuLocalInterrupts::save_exit(&mut self.0))?;
        Ok(exit)
    }

    fn bind(&mut self) -> BackendResult {
        let _irqs = IrqSaveGuard::new();
        x86_result(self.0.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        let _irqs = IrqSaveGuard::new();
        x86_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        x86_result(self.0.inject_interrupt(vector))
    }

    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> BackendResult {
        x86_result(VcpuLocalInterrupts::inject(
            &mut self.0,
            PendingVcpuInterrupt {
                id: VirtualInterruptId(vector as u32),
                trigger,
                source: None,
            },
        ))
    }

    fn handle_eoi(&mut self) -> Option<u8> {
        self.0.handle_eoi()
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

#[cfg(test)]
const fn x86_interrupt_is_level_triggered(trigger: InterruptTriggerMode) -> bool {
    match trigger {
        InterruptTriggerMode::EdgeTriggered => false,
        InterruptTriggerMode::LevelTriggered => true,
    }
}

pub(crate) struct AxvmX86PerCpu(ax_cpu::virtualization::PerCpu<control_memory::ControlPages>);

impl VmArchPerCpuOps for AxvmX86PerCpu {
    fn new(_cpu_id: usize) -> BackendResult<Self> {
        let memory = control_memory::ControlPages::allocate(1)?;
        let cpu =
            ax_cpu::virtualization::PerCpu::new(memory).map_err(|_| BackendError::Unsupported)?;
        let format = x86_result(crate::arch::x86_64::policy::selected_nested_paging_format())?;
        if !matches!(
            (cpu.backend(), format),
            (
                ax_cpu::virtualization::Backend::Vmx,
                X86NestedPagingFormat::Ept
            ) | (
                ax_cpu::virtualization::Backend::Svm,
                X86NestedPagingFormat::Npt
            )
        ) {
            return Err(BackendError::Unsupported);
        }
        Ok(Self(cpu))
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> BackendResult {
        let _irqs = IrqSaveGuard::new();
        // SAFETY: AxVM's per-CPU initialization owns this physical CPU before
        // guests can be scheduled. Its prepared control lease stays with self.
        unsafe {
            if self.0.backend() == ax_cpu::virtualization::Backend::Vmx {
                ax_cpu::boot::authorize_vmx().map_err(|_| BackendError::Unsupported)?;
            }
            self.0.enable().map_err(|_| BackendError::InvalidState)
        }
    }

    fn hardware_disable(&mut self) -> BackendResult {
        let _irqs = IrqSaveGuard::new();
        // SAFETY: AxVM retires every guest binding on this CPU before teardown.
        // A failure keeps the hardware owner and its memory lease active.
        unsafe { self.0.disable() }.map_err(|_| BackendError::InvalidState)
    }
}

/// Provides the canonical x86 interrupt-controller device model.
pub(crate) fn ioapic_model(
    vm_id: usize,
    base: usize,
    length: usize,
    binding: Arc<X86RunBinding>,
) -> Arc<dyn DeviceModel> {
    Arc::new(X86IoApicModel {
        vm_id,
        base,
        length,
        binding,
    })
}

pub(crate) fn unassigned_mmio_model(base: usize, length: usize) -> Arc<dyn DeviceModel> {
    Arc::new(X86UnassignedMmioModel { base, length })
}

pub(crate) fn pit_model(vm_id: usize, binding: Arc<X86RunBinding>) -> Arc<dyn DeviceModel> {
    Arc::new(X86PitModel { vm_id, binding })
}

struct X86IoApicModel {
    vm_id: usize,
    base: usize,
    length: usize,
    /// Run binding shared with every vCPU-owned interrupt device of this VM.
    binding: Arc<X86RunBinding>,
}

struct X86UnassignedMmioModel {
    base: usize,
    length: usize,
}

/// Lower run-bound interrupt delivery port.
///
/// Owns only the PIC/IOAPIC projection, the bounded host-forwarding state and
/// the run signal/IPI binding. It is the only x86 interrupt state a prepared
/// [`X86Entry`] retains, so no loaded path can reach the task-side registration
/// maps or their sleeping-capable mutexes.
pub(super) struct X86DeliveryPort {
    vm_id: usize,
    /// Shared port that every vCPU-owned x86 interrupt device binds at entry.
    binding: Arc<X86RunBinding>,
    wired: Arc<X86WiredState>,
    /// Lower controller state read from hard-IRQ forwarding hooks. Fixed bounded
    /// storage, no allocation, wake, IPI or callback while guarded.
    forwarding: RawSpinLock<irq::X86IoApicForwardingState>,
    /// Monotonic delivery identity shared by the task-side EOI producer and
    /// controller owner. Saturation is retained rather than wrapping so a
    /// retired token can never alias a later delivery.
    delivery_sequence: AtomicU64,
}

impl X86DeliveryPort {
    fn new(vm_id: usize, ioapic: Arc<dyn X86IoApicDeviceOps>, binding: Arc<X86RunBinding>) -> Self {
        Self {
            vm_id,
            wired: Arc::new(X86WiredState {
                ioapic,
                pending: AtomicUsize::new(0),
                pending_level: AtomicUsize::new(0),
                binding: Arc::clone(&binding),
                endpoint: OnceLock::new(),
            }),
            binding,
            forwarding: RawSpinLock::new(irq::X86IoApicForwardingState::new()),
            delivery_sequence: AtomicU64::new(1),
        }
    }

    /// Lower forwarding state; interrupts are saved so a hard-IRQ hook can
    /// never observe (or deadlock against) a task-context holder.
    fn forwarding(&self) -> RawSpinLockIrqSaveGuard<'_, irq::X86IoApicForwardingState> {
        self.forwarding.lock_irqsave()
    }

    fn install_endpoint(&self, endpoint: Weak<X86InterruptDomain>) {
        let _ = self.wired.endpoint.set(endpoint);
    }

    fn take_pending_wired_gsis(&self) -> (usize, usize) {
        let pending = self.wired.pending.swap(0, Ordering::AcqRel);
        let pending_level = self
            .wired
            .pending_level
            .fetch_and(!pending, Ordering::AcqRel);
        (pending, pending_level & pending)
    }

    /// VM identity fixed when this run-scoped port was created.
    pub(super) fn vm_id(&self) -> usize {
        self.vm_id
    }

    /// Shared port that every vCPU-owned x86 interrupt device binds at entry.
    pub(super) fn run_binding(&self) -> Arc<X86RunBinding> {
        Arc::clone(&self.binding)
    }

    fn current_signals(&self) -> Option<Arc<RunSignals>> {
        self.binding.current_signals()
    }

    fn publish_interrupt(
        &self,
        signals: &Arc<RunSignals>,
        target_vcpu_id: usize,
        interrupt: IoApicInterrupt,
        source: InterruptSourceId,
    ) -> AxVmResult {
        signals
            .publish(
                target_vcpu_id,
                PendingVcpuInterrupt {
                    id: VirtualInterruptId(interrupt.vector.into()),
                    trigger: if interrupt.level_triggered {
                        InterruptTriggerMode::LevelTriggered
                    } else {
                        InterruptTriggerMode::EdgeTriggered
                    },
                    source: Some(source),
                },
            )
            .map_err(|error| AxVmError::interrupt("publish x86 interrupt", error))?;
        signals
            .kick(target_vcpu_id)
            .map_err(|error| AxVmError::interrupt("kick x86 vCPU", error))
    }

    fn vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        self.wired.ioapic.vector_for_gsi(gsi)
    }

    fn assert_gsi(&self, gsi: usize) -> Option<x86_vlapic::IoApicInterrupt> {
        self.wired.ioapic.assert_gsi(gsi)
    }

    fn end_of_interrupt(&self, vector: u8) -> Option<x86_vlapic::IoApicEoi> {
        self.wired.ioapic.end_of_interrupt(vector)
    }

    fn source_for_vector(&self, vector: u8) -> Option<InterruptSourceId> {
        (0..irq::IOAPIC_GSI_COUNT)
            .find(|&gsi| self.vector_for_gsi(gsi) == Some(vector))
            .map(|gsi| InterruptSourceId::new(InterruptControllerId::new(0), gsi as u32, None))
    }

    fn next_delivery_sequence(&self) -> u64 {
        self.delivery_sequence
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or(u64::MAX)
    }
}

/// Task-side x86 interrupt domain.
///
/// Keeps the registration maps and host IRQ hook list, which are only ever
/// touched by the owning task, next to the lower [`X86DeliveryPort`] the entry
/// and hard-IRQ hooks actually use.
pub(crate) struct X86InterruptDomain {
    port: Arc<X86DeliveryPort>,
    /// Task-only input resolver: registered guest GSI inputs never change in
    /// hard IRQ, so this is a genuine sleeping-capable `std::sync::Mutex`.
    inputs: Mutex<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
    /// Task-only lease list for host IRQ hooks.
    forwarding_hooks: Mutex<std::vec::Vec<host_irq::IrqHandle>>,
}

struct X86WiredState {
    ioapic: Arc<dyn X86IoApicDeviceOps>,
    pending: AtomicUsize,
    pending_level: AtomicUsize,
    binding: Arc<X86RunBinding>,
    endpoint: OnceLock<Weak<X86InterruptDomain>>,
}

/// Private key for the concrete VM-owned x86 forwarding domain.
///
/// The public `X86InterruptDomainKey` exposes only injection operations. This
/// key is intentionally architecture-private because hook ownership and
/// teardown are runtime implementation details.
pub(crate) struct X86InterruptDomainRuntimeKey;

pub(crate) fn apply_interrupt_event(
    devices: &Arc<axdevice::DeviceRuntime>,
    event: SourceEvent,
) -> AxVmResult {
    let owner = devices
        .services()
        .require::<X86InterruptDomainRuntimeKey>()
        .map_err(|error| AxVmError::device("resolve x86 interrupt owner", error))?;
    InterruptControllerOwner::apply_source(owner.as_ref(), event)
        .map_err(|error| AxVmError::interrupt("apply x86 interrupt event", error))
}

impl ServiceKey for X86InterruptDomainRuntimeKey {
    type Service = X86InterruptDomain;

    const NAME: &'static str = "x86-interrupt-domain-runtime";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

impl X86InterruptDomain {
    fn inputs(&self) -> MutexGuard<'_, BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>> {
        self.inputs.lock_unpoisoned()
    }

    fn forwarding_hooks(&self) -> MutexGuard<'_, std::vec::Vec<host_irq::IrqHandle>> {
        self.forwarding_hooks.lock_unpoisoned()
    }

    fn new(vm_id: usize, ioapic: Arc<dyn X86IoApicDeviceOps>, binding: Arc<X86RunBinding>) -> Self {
        Self {
            port: Arc::new(X86DeliveryPort::new(vm_id, ioapic, binding)),
            inputs: Mutex::new(BTreeMap::new()),
            forwarding_hooks: Mutex::new(std::vec::Vec::new()),
        }
    }

    fn install_endpoint(&self, endpoint: Weak<X86InterruptDomain>) {
        self.port.install_endpoint(endpoint);
    }

    /// Lower port retained by prepared entries and hard-IRQ forwarding hooks.
    pub(super) fn delivery_port(&self) -> Arc<X86DeliveryPort> {
        Arc::clone(&self.port)
    }

    pub(super) fn add_forwarding_hook(&self, hook: host_irq::IrqHandle) {
        self.forwarding_hooks().push(hook);
    }

    pub(super) fn take_forwarding_hooks(&self) -> std::vec::Vec<host_irq::IrqHandle> {
        std::mem::take(&mut *self.forwarding_hooks())
    }

    /// VM identity fixed when this run-scoped domain was created.
    pub(super) fn vm_id(&self) -> usize {
        self.port.vm_id()
    }

    /// Shared port that every vCPU-owned x86 interrupt device binds at entry.
    pub(super) fn run_binding(&self) -> Arc<X86RunBinding> {
        self.port.run_binding()
    }
}

impl X86InterruptDomainOps for X86InterruptDomain {
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        self.port.vector_for_gsi(gsi)
    }

    fn assert_gsi(&self, gsi: usize) -> Option<x86_vlapic::IoApicInterrupt> {
        self.port.assert_gsi(gsi)
    }

    fn end_of_interrupt(&self, vector: u8) -> Option<x86_vlapic::IoApicEoi> {
        self.port.end_of_interrupt(vector)
    }
}

impl VirtualInterruptController for X86InterruptDomain {
    fn id(&self) -> InterruptControllerId {
        InterruptControllerId::new(0)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        let gsi = input.value();
        if gsi >= irq::IOAPIC_GSI_COUNT {
            return Err(IrqError::InvalidInput {
                endpoint: InterruptEndpoint::Wired {
                    controller: <Self as VirtualInterruptController>::id(self),
                    input,
                },
                operation: "open x86 IOAPIC input",
                detail: std::format!("GSI {gsi} is outside 0..{}", irq::IOAPIC_GSI_COUNT),
            });
        }
        let mut inputs = self.inputs();
        if let Some((registered_trigger, registered)) = inputs.get(&gsi) {
            if *registered_trigger != trigger {
                return Err(IrqError::InvalidInput {
                    endpoint: InterruptEndpoint::Wired {
                        controller: <Self as VirtualInterruptController>::id(self),
                        input,
                    },
                    operation: "open x86 IOAPIC input",
                    detail: std::format!(
                        "GSI {gsi} is already registered as {registered_trigger:?}"
                    ),
                });
            }
            return Ok(registered.clone());
        }
        let sink: Arc<dyn WiredIrqSink> = self.port.wired.clone();
        let registered = WiredIrqInput::new(
            <Self as VirtualInterruptController>::id(self),
            input,
            trigger,
            sink,
        );
        inputs.insert(gsi, (trigger, registered.clone()));
        Ok(registered)
    }
}

impl InterruptControllerEndpoint for X86InterruptDomain {
    type Error = AxVmError;

    fn id(&self) -> InterruptControllerId {
        <Self as VirtualInterruptController>::id(self)
    }

    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput> {
        <Self as VirtualInterruptController>::wired_input(self, input, trigger)
    }

    fn submit(&self, event: SourceEvent) -> AxVmResult {
        let epoch = match event {
            SourceEvent::Pulse { epoch, .. }
            | SourceEvent::Level { epoch, .. }
            | SourceEvent::Eoi { epoch, .. } => epoch,
        };
        let signals = self
            .port
            .current_signals()
            .ok_or_else(|| AxVmError::interrupt("submit x86 interrupt", "no active run"))?;
        if signals.epoch() != epoch {
            return Err(AxVmError::StaleRun {
                expected: epoch.run(),
                current: Some(signals.run_id()),
            });
        }
        signals
            .publish_controller_event(event)
            .map_err(|error| AxVmError::interrupt("queue x86 interrupt event", error))
    }
}

impl InterruptControllerOwner for X86InterruptDomain {
    type Error = AxVmError;

    fn apply_source(&self, event: SourceEvent) -> AxVmResult {
        let epoch = match event {
            SourceEvent::Pulse { epoch, .. }
            | SourceEvent::Level { epoch, .. }
            | SourceEvent::Eoi { epoch, .. } => epoch,
        };
        let signals = self
            .port
            .current_signals()
            .ok_or_else(|| AxVmError::interrupt("submit x86 interrupt", "no active run"))?;
        if signals.epoch() != epoch {
            return Err(AxVmError::StaleRun {
                expected: epoch.run(),
                current: Some(signals.run_id()),
            });
        }

        let source = match event {
            SourceEvent::Pulse { source, .. } | SourceEvent::Level { source, .. } => source,
            SourceEvent::Eoi { token, .. } => token.source,
        };
        if source.controller != <Self as InterruptControllerEndpoint>::id(self) {
            return Err(AxVmError::invalid_input(
                "submit x86 interrupt",
                "source belongs to another interrupt controller",
            ));
        }
        let gsi = usize::try_from(source.source).map_err(|_| {
            AxVmError::invalid_input("submit x86 interrupt", "GSI does not fit usize")
        })?;
        if gsi >= irq::IOAPIC_GSI_COUNT {
            return Err(AxVmError::invalid_input(
                "submit x86 interrupt",
                "GSI is outside the virtual IOAPIC range",
            ));
        }

        match event {
            SourceEvent::Pulse { .. } => {
                if let Some(interrupt) = self.port.assert_gsi(gsi) {
                    self.port
                        .publish_interrupt(&signals, 0, interrupt, source)?;
                }
            }
            SourceEvent::Level { asserted, .. } => {
                if let Some(interrupt) = self.port.wired.ioapic.set_gsi_level(gsi, asserted) {
                    self.port
                        .publish_interrupt(&signals, 0, interrupt, source)?;
                }
            }
            SourceEvent::Eoi { token, .. } => {
                if token.target.run != epoch.run || !signals.is_current_instance(token.target) {
                    return Err(AxVmError::invalid_input(
                        "complete x86 interrupt",
                        "delivery token belongs to another run or vCPU activation",
                    ));
                }
                let vector = self.port.vector_for_gsi(gsi).ok_or_else(|| {
                    AxVmError::invalid_input(
                        "complete x86 interrupt",
                        "GSI is masked or unconfigured",
                    )
                })?;
                if let Some(completion) = self.port.end_of_interrupt(vector) {
                    irq::rearm_forwarded_host_gsi_after_eoi(
                        &self.port,
                        completion.gsi,
                        completion.pending,
                    );
                    if let Some(interrupt) = completion.pending {
                        self.port.publish_interrupt(
                            &signals,
                            token.target.vcpu_id,
                            interrupt,
                            source,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl X86WiredState {
    /// Wakes the boot vCPU after publishing one wired GSI edge.
    ///
    /// The GSI bit is already canonical above, so a target that is not yet
    /// registered is not a failure: it drains this bit at its first entry.
    fn wake_boot_vcpu_from_irq(&self) {
        let _ = self.binding.kick_from_irq(0);
    }

    fn publish(
        &self,
        input: ControllerInputId,
        interrupt: x86_vlapic::IoApicInterrupt,
    ) -> IrqResult {
        let bit = 1usize << input.value();
        if interrupt.level_triggered {
            self.pending_level.fetch_or(bit, Ordering::Release);
        }
        self.pending.fetch_or(bit, Ordering::Release);
        match self.binding.kick_from_irq(0) {
            Ok(()) | Err(SignalError::InactiveTarget) => Ok(()),
            Err(error) => Err(IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: InterruptControllerId::new(0),
                    input,
                },
                operation: "publish x86 IOAPIC vCPU kick",
                detail: std::format!("{error}"),
            }),
        }
    }
}

impl WiredIrqSink for X86WiredState {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        if let Some(domain) = self.endpoint.get().and_then(Weak::upgrade)
            && let Some(signals) = domain.port.current_signals()
        {
            return InterruptControllerEndpoint::submit(
                domain.as_ref(),
                SourceEvent::Level {
                    epoch: signals.epoch(),
                    source: InterruptSourceId::new(
                        <X86InterruptDomain as VirtualInterruptController>::id(domain.as_ref()),
                        input.value() as u32,
                        None,
                    ),
                    asserted,
                },
            )
            .map_err(|error| IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: InterruptControllerId::new(0),
                    input,
                },
                operation: "submit x86 IOAPIC level event",
                detail: std::format!("{error}"),
            });
        }
        if let Some(interrupt) = self.ioapic.set_gsi_level(input.value(), asserted) {
            self.publish(input, interrupt)?;
        }
        Ok(())
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        if let Some(domain) = self.endpoint.get().and_then(Weak::upgrade)
            && let Some(signals) = domain.port.current_signals()
        {
            return InterruptControllerEndpoint::submit(
                domain.as_ref(),
                SourceEvent::Pulse {
                    epoch: signals.epoch(),
                    source: InterruptSourceId::new(
                        <X86InterruptDomain as VirtualInterruptController>::id(domain.as_ref()),
                        input.value() as u32,
                        None,
                    ),
                },
            )
            .map_err(|error| IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: InterruptControllerId::new(0),
                    input,
                },
                operation: "submit x86 IOAPIC pulse event",
                detail: std::format!("{error}"),
            });
        }
        if let Some(interrupt) = self.ioapic.assert_gsi(input.value()) {
            self.publish(input, interrupt)?;
        }
        Ok(())
    }
}

impl DeviceModel for X86IoApicModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        fixed_mmio_declaration(self.base, self.length, "declare x86 virtual IOAPIC")
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::interfaces(
            None,
            Some(std::vec![AcpiContributionSpec::InterruptController {
                controller: axdevice_base::InterruptControllerId::new(0),
                device: AcpiDeviceSpec::table("IOAP").with_register(
                    ResourceSlot::new("registers").expect("static IOAPIC slot is valid"),
                ),
            }]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) =
            consume_mmio_config(context, self.base, self.length, "build x86 virtual IOAPIC")?;
        let ioapic = Arc::new(axdevice::X86IoApicDevice::new(
            x86_vlapic::X86GuestPhysAddr::from_usize(base),
            Some(length),
        ));
        let service: Arc<dyn X86IoApicDeviceOps> = ioapic.clone();
        let runtime = Arc::new(X86InterruptDomain::new(
            self.vm_id,
            service.clone(),
            Arc::clone(&self.binding),
        ));
        runtime.install_endpoint(Arc::downgrade(&runtime));
        let domain: Arc<dyn X86InterruptDomainOps> = runtime.clone();
        let controller: Arc<dyn VirtualInterruptController> = runtime.clone();
        let mut bundle = DeviceBundle::from_registration(DeviceRegistration::Device(ioapic))
            .with_service::<X86IoApicServiceKey>(service)?;
        bundle.push(DeviceRegistration::InterruptController(
            ControllerRegistration::new(
                <X86InterruptDomain as VirtualInterruptController>::id(&runtime),
                controller,
            ),
        ));
        bundle
            .with_service::<X86InterruptDomainKey>(domain)?
            .with_service::<X86InterruptDomainRuntimeKey>(runtime)
    }
}

impl DeviceModel for X86UnassignedMmioModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        fixed_mmio_declaration(self.base, self.length, "declare x86 unassigned MMIO window")
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::None
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = consume_mmio_config(
            context,
            self.base,
            self.length,
            "build x86 unassigned MMIO window",
        )?;
        let device = axdevice::X86UnassignedMmioDevice::new(base as u64, length as u64)?;
        Ok(DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(device),
        )))
    }
}

struct X86PitModel {
    vm_id: usize,
    /// Run binding shared with the IOAPIC interrupt domain and every vCPU.
    binding: Arc<X86RunBinding>,
}

impl DeviceModel for X86PitModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        let [timer, speaker] = x86_vlapic::EmulatedPit::<AxvmX86HostOps>::port_ranges();
        let timer_size = timer.end.number() - timer.start.number() + 1;
        let speaker_size = speaker.end.number() - speaker.start.number() + 1;
        DeviceRequirements::new()
            .with_pio(
                ResourceSlot::new("timer-registers")?,
                timer_size,
                1,
                ResourceRequest::Fixed(timer.start.number()),
            )?
            .with_pio(
                ResourceSlot::new("speaker-control")?,
                speaker_size,
                1,
                ResourceRequest::Fixed(speaker.start.number()),
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::interfaces(
            None,
            Some(std::vec![AcpiContributionSpec::Timer(
                AcpiDeviceSpec::new("PIT0", "PNP0100")
                    .with_register(
                        ResourceSlot::new("timer-registers").expect("static PIT slot is valid"),
                    )
                    .with_register(
                        ResourceSlot::new("speaker-control").expect("static PIT slot is valid"),
                    ),
            )]),
        )
    }

    fn build(&self, context: &mut DeviceBuildContext<'_>) -> DeviceManagerResult<DeviceBundle> {
        let timer = context.pio(&ResourceSlot::new("timer-registers")?)?;
        let speaker = context.pio(&ResourceSlot::new("speaker-control")?)?;
        let [expected_timer, expected_speaker] =
            x86_vlapic::EmulatedPit::<AxvmX86HostOps>::port_ranges();
        if timer != pio_range_parts(expected_timer) || speaker != pio_range_parts(expected_speaker)
        {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "build x86 virtual PIT",
                detail: "planned port ranges differ from the PIT hardware model".into(),
            });
        }
        // The PIT programs IRQ0 from the guest, so its port must already be
        // bound to this run's signal target: a timer that fires must retain the
        // original run binding instead of looking up a VM identity.
        let runtime = AxvmX86VlapicRuntime::new(self.vm_id, 0, Arc::clone(&self.binding));
        let pit = Arc::new(
            axdevice::X86PitDevice::<AxvmX86HostOps>::new_for_vcpu_with_runtime(
                runtime, self.vm_id, 0,
            ),
        );
        // The PIT IRQ0 host timer is a task-side producer, so it registers the
        // same device as its lifecycle owner: pause quiesces it before the
        // pause is ACKed and resume reinstalls it before guest entry reopens.
        let device: Arc<dyn Device> = pit.clone();
        let lifecycle: Arc<dyn DeviceLifecycle> = pit;
        Ok(
            DeviceBundle::from_registration(DeviceRegistration::Device(device))
                .with_lifecycle(lifecycle),
        )
    }
}

fn pio_range_parts(range: x86_vlapic::X86PortRange) -> (u16, u16) {
    (
        range.start.number(),
        range.end.number() - range.start.number() + 1,
    )
}

fn consume_mmio_config(
    context: &mut DeviceBuildContext<'_>,
    expected_base: usize,
    expected_length: usize,
    operation: &'static str,
) -> DeviceManagerResult<(usize, usize)> {
    let (base, length) = context.mmio(&ResourceSlot::new("registers")?)?;
    if base != expected_base as u64 || length != expected_length as u64 {
        return Err(DeviceManagerError::InvalidConfig {
            operation,
            detail: "planned MMIO range differs from the machine descriptor".into(),
        });
    }
    Ok((
        usize::try_from(base).map_err(|_| declaration_range_error(operation))?,
        usize::try_from(length).map_err(|_| declaration_range_error(operation))?,
    ))
}

fn fixed_mmio_declaration(
    base: usize,
    length: usize,
    operation: &'static str,
) -> DeviceManagerResult<DeviceRequirements> {
    let size = u64::try_from(length).map_err(|_| declaration_range_error(operation))?;
    let base = u64::try_from(base).map_err(|_| declaration_range_error(operation))?;
    DeviceRequirements::new().with_mmio(
        ResourceSlot::new("registers")?,
        size,
        1,
        ResourceRequest::Fixed(base),
    )
}

fn declaration_range_error(operation: &'static str) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation,
        detail: "configured address or length exceeds the selected bus width".into(),
    }
}

pub(crate) fn x86_apic_access_page_addr() -> AxVmResult<axvm_types::HostPhysAddr> {
    x86_result(crate::arch::x86_64::policy::apic_access_page_addr::<
        AxvmX86HostOps,
    >())
    .map(|addr| axvm_types::HostPhysAddr::from(addr.as_usize()))
    .map_err(|error| AxVmError::vcpu("get x86 APIC access page", error))
}

pub(crate) fn x86_apic_access_page_gpa() -> AxVmResult<axvm_types::GuestPhysAddr> {
    x86_result(crate::arch::x86_64::policy::apic_access_page_gpa())
        .map(|addr| axvm_types::GuestPhysAddr::from(addr.as_usize()))
        .map_err(|error| AxVmError::vcpu("get x86 APIC access page", error))
}

pub(crate) fn x86_requires_apic_access_page() -> AxVmResult<bool> {
    x86_result(crate::arch::x86_64::policy::requires_apic_access_page())
        .map_err(|error| AxVmError::vcpu("check x86 APIC access page", error))
}

fn x86_result<T>(result: X86VcpuResult<T>) -> BackendResult<T> {
    result.map_err(x86_error_to_backend)
}

fn x86_error_to_backend(err: X86VcpuError) -> BackendError {
    match err {
        X86VcpuError::InvalidInput => BackendError::InvalidInput,
        X86VcpuError::InvalidData => BackendError::InvalidData,
        X86VcpuError::Unsupported => BackendError::Unsupported,
        X86VcpuError::BadState => BackendError::InvalidState,
        X86VcpuError::NoMemory => BackendError::OutOfMemory,
        X86VcpuError::ResourceBusy => BackendError::ResourceBusy,
        X86VcpuError::TimerUnavailable => BackendError::InvalidState,
    }
}

fn ax_guest_phys_addr_to_x86(addr: GuestPhysAddr) -> X86GuestPhysAddr {
    X86GuestPhysAddr::from_usize(addr.as_usize())
}

fn x86_guest_phys_addr_to_ax(addr: X86GuestPhysAddr) -> GuestPhysAddr {
    GuestPhysAddr::from(addr.as_usize())
}

fn ax_nested_paging_to_x86(config: NestedPagingConfig) -> X86NestedPagingConfig {
    X86NestedPagingConfig::new(
        X86HostPhysAddr::from_usize(config.root_paddr.as_usize()),
        config.levels,
        config.gpa_bits,
        config.mode,
    )
}

fn x86_access_width_to_ax(width: X86AccessWidth) -> AccessWidth {
    match width {
        X86AccessWidth::Byte => AccessWidth::Byte,
        X86AccessWidth::Word => AccessWidth::Word,
        X86AccessWidth::Dword => AccessWidth::Dword,
        X86AccessWidth::Qword => AccessWidth::Qword,
    }
}

fn x86_access_flags_to_ax(flags: X86AccessFlags) -> MappingFlags {
    let mut out = MappingFlags::empty();
    if flags.contains(X86AccessFlags::READ) {
        out |= MappingFlags::READ;
    }
    if flags.contains(X86AccessFlags::WRITE) {
        out |= MappingFlags::WRITE;
    }
    if flags.contains(X86AccessFlags::EXECUTE) {
        out |= MappingFlags::EXECUTE;
    }
    out
}

fn x86_port_to_ax(port: X86Port) -> Port {
    Port::new(port.number())
}

fn x86_msr_addr_to_ax(addr: X86MsrAddr) -> SysRegAddr {
    SysRegAddr::new(addr.addr())
}

fn current_rflags() -> u64 {
    let flags: u64;
    unsafe {
        asm!(
            "pushfq",
            "pop {flags}",
            flags = lateout(reg) flags,
            options(nomem, preserves_flags),
        );
    }
    flags
}

fn restore_host_interrupt_flag(host_rflags: u64) {
    if host_rflags & RFLAGS_INTERRUPT_FLAG != 0 {
        unsafe {
            asm!("sti", options(nomem, nostack));
        }
    } else {
        unsafe {
            asm!("cli", options(nomem, nostack));
        }
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[test]
    fn pit_fans_out_gsi_zero_to_pic_and_ioapic() {
        let pic_pulses = Cell::new(0);
        let ioapic_asserts = Cell::new(0);
        let injected = std::cell::RefCell::new(std::vec::Vec::new());

        route_pit_irq(
            || {
                pic_pulses.set(pic_pulses.get() + 1);
                Some(0x20)
            },
            || {
                ioapic_asserts.set(ioapic_asserts.get() + 1);
                Some(IoApicInterrupt {
                    vector: 0x20,
                    level_triggered: false,
                })
            },
            |vector, trigger, source| {
                injected.borrow_mut().push((vector, trigger, source));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(pic_pulses.get(), 1);
        assert_eq!(ioapic_asserts.get(), 1);
        assert_eq!(
            injected.into_inner(),
            [
                (
                    0x20,
                    InterruptTriggerMode::EdgeTriggered,
                    PitInterruptSource::LegacyPic,
                ),
                (
                    0x20,
                    InterruptTriggerMode::EdgeTriggered,
                    PitInterruptSource::IoApic,
                ),
            ]
        );
    }

    #[test]
    fn pit_keeps_pic_delivery_when_ioapic_defers_the_edge() {
        let injected = Cell::new(None);

        route_pit_irq(
            || Some(0x20),
            || None,
            |vector, trigger, source| {
                injected.set(Some((vector, trigger, source)));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            injected.get(),
            Some((
                0x20,
                InterruptTriggerMode::EdgeTriggered,
                PitInterruptSource::LegacyPic,
            ))
        );
    }

    #[test]
    fn pit_preserves_the_ioapic_trigger_mode() {
        let injected_trigger = Cell::new(None);

        route_pit_irq(
            || None,
            || {
                Some(IoApicInterrupt {
                    vector: 0x30,
                    level_triggered: true,
                })
            },
            |_vector, trigger, source| {
                assert_eq!(source, PitInterruptSource::IoApic);
                injected_trigger.set(Some(trigger));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            injected_trigger.get(),
            Some(InterruptTriggerMode::LevelTriggered)
        );
    }

    #[test]
    fn failed_pic_dispatch_restores_the_claim() {
        let restored = Cell::new(None);

        let result = route_pit_claim(
            || Some(0x68),
            |vector| *vector,
            |claim| restored.set(Some(claim)),
            || None,
            |_vector, _trigger, _source| Err(X86VlapicError::BadState),
        );

        assert_eq!(result, Err(X86VlapicError::BadState));
        assert_eq!(restored.get(), Some(0x68));
    }

    #[test]
    fn converts_x86_vcpu_errors_to_backend_errors() {
        assert_eq!(
            x86_error_to_backend(X86VcpuError::InvalidInput),
            BackendError::InvalidInput
        );
        assert_eq!(
            x86_error_to_backend(X86VcpuError::NoMemory),
            BackendError::OutOfMemory
        );
        assert_eq!(
            x86_error_to_backend(X86VcpuError::ResourceBusy),
            BackendError::ResourceBusy
        );
    }

    #[test]
    fn converts_x86_value_types_to_axvm_value_types() {
        assert_eq!(
            x86_guest_phys_addr_to_ax(X86GuestPhysAddr::from_usize(0x4000)).as_usize(),
            0x4000
        );
        assert_eq!(
            x86_access_width_to_ax(X86AccessWidth::Dword),
            AccessWidth::Dword
        );
        assert_eq!(x86_port_to_ax(X86Port::new(0x3f8)).0, 0x3f8);
        assert_eq!(x86_msr_addr_to_ax(X86MsrAddr::new(0x800)).0, 0x800);
    }

    #[test]
    fn maps_edge_and_level_triggers_to_x86_backend_modes() {
        assert!(!x86_interrupt_is_level_triggered(
            InterruptTriggerMode::EdgeTriggered
        ));
        assert!(x86_interrupt_is_level_triggered(
            InterruptTriggerMode::LevelTriggered
        ));
    }
}
