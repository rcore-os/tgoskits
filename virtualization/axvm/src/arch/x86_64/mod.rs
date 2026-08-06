//! AxVM x86_64 adapter.
//!
//! This module owns the AxVM/ArceOS glue for the OS-neutral `x86_vcpu` and
//! `x86_vlapic` cores.

use std::{
    arch::asm,
    boxed::Box,
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use ax_std::os::arceos::sync::RawSpinLock;
use axdevice::{
    DeviceBuildContext, DeviceBundle, DeviceFactory, DeviceFactoryRegistry, DeviceManagerResult,
    DeviceRegistration, ServiceCardinality, ServiceKey, VirtualInterruptControllerKey,
    X86InterruptDomainKey, X86InterruptDomainOps, X86IoApicDeviceOps, X86IoApicServiceKey,
    X86PitDeviceOps, X86PitServiceKey, validate_device_config,
};
use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptEndpoint, IrqError, IrqResult,
    VirtualInterruptController, WiredIrqInput, WiredIrqSink,
};
use axvm_types::{
    AccessWidth, EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, InterruptTriggerMode,
    MappingFlags, NestedPagingConfig, Port, SysRegAddr, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError as BackendError, VmBackendResult as BackendResult,
};
use x86_vcpu::{
    X86AccessFlags, X86AccessWidth, X86GuestPhysAddr, X86HostOps, X86HostPhysAddr, X86HostVirtAddr,
    X86MsrAddr, X86NestedPagingConfig, X86PerCpuState, X86Port, X86Vcpu, X86VcpuCreateConfig,
    X86VcpuError, X86VcpuResult, X86VcpuSetupConfig, X86VmExit,
};
use x86_vlapic::{
    X86InterruptVector, X86TimerCallback, X86VcpuId, X86VlapicError, X86VlapicHostOps,
    X86VlapicResult, X86VmId,
};

use super::{ArchOps, BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction};
use crate::{
    AxVmError, AxVmResult, StopReason,
    host::{HostMemory, default_host},
    irq::deferred::DeferredVcpuKick,
    manager,
    vcpu::with_current_vcpu,
};

pub(crate) mod boot;
mod capabilities;
mod exit;
pub(crate) mod fdt;
mod host_irq;
pub(crate) mod irq;
mod nested_paging;
pub(crate) mod port;
#[path = "../../architecture/sysreg.rs"]
mod sysreg;
mod vm;

use exit::{DeferredRunWork, IoReadExit, IoWriteExit, NestedPageFaultExit};
use sysreg::{SysRegReadExit, SysRegWriteExit};

const QEMU_EXIT_PORT: u16 = 0x604;
const QEMU_EXIT_MAGIC: u64 = 0x2000;
const RFLAGS_INTERRUPT_FLAG: u64 = 1 << 9;

pub(crate) struct X86_64Arch;

impl ArchOps for X86_64Arch {
    type VCpu = AxvmX86Vcpu;
    type PerCpu = AxvmX86PerCpu;
    type DeferredRunWork = DeferredRunWork;
    type NestedPageTable = nested_paging::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        x86_vcpu::initialize_hardware_support().is_ok()
    }

    fn before_first_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        irq::start_deferred_irq_delivery(vm);
        irq::enable_ioapic_irq_forwarding(vm, vcpu);
    }

    fn before_vcpu_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) -> AxVmResult {
        irq::drain_pending_wired_irqs(vm, vcpu);
        irq::drain_pending_ioapic_irqs(vm, vcpu);
        irq::activate_ready_ioapic_forwarding_routes(vm);
        Ok(())
    }

    fn after_external_interrupt(
        _vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        vector: usize,
    ) {
        crate::host::arceos::dispatch_host_irq(vector);
        crate::check_timer_events();
    }

    fn on_last_vcpu_exit(vm: &crate::AxVMRef) -> AxVmResult {
        irq::disable_ioapic_irq_forwarding_for_vm(vm);
        irq::stop_deferred_irq_delivery(vm);
        Ok(())
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
            X86VmExit::Hypercall { nr, args } => super::handle_hypercall(
                vm,
                vcpu,
                HypercallExit { nr, args },
                crate::runtime::hvc::HyperCallAbi::Generic,
            ),
            X86VmExit::PortIoRead { port, width } => exit::handle_io_read(
                vm,
                vcpu,
                IoReadExit {
                    port: x86_port_to_ax(port),
                    width: x86_access_width_to_ax(width),
                },
            ),
            X86VmExit::PortIoWrite { port, width, data } => {
                if x86_qemu_shutdown_port(port, width, data) {
                    warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                    Ok(BoundVcpuExit::Complete(VcpuRunAction {
                        waits_for_event: false,
                        stop_reason: Some(StopReason::SystemDown),
                        resets_vm: false,
                        exits_vcpu: false,
                    }))
                } else {
                    exit::handle_io_write(
                        vm,
                        IoWriteExit {
                            port: x86_port_to_ax(port),
                            width: x86_access_width_to_ax(width),
                            data,
                        },
                    )
                }
            }
            X86VmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => super::handle_mmio_read(
                vm,
                vcpu,
                MmioReadExit {
                    addr: x86_guest_phys_addr_to_ax(addr),
                    width: x86_access_width_to_ax(width),
                    reg,
                    reg_width: x86_access_width_to_ax(reg_width),
                    signed_ext,
                },
            ),
            X86VmExit::MmioWrite { addr, width, data } => super::handle_mmio_write::<Self>(
                vm,
                MmioWriteExit {
                    addr: x86_guest_phys_addr_to_ax(addr),
                    width: x86_access_width_to_ax(width),
                    data,
                },
            ),
            X86VmExit::MsrRead { addr } => sysreg::handle_read(
                vm,
                vcpu,
                SysRegReadExit {
                    addr: x86_msr_addr_to_ax(addr),
                    reg: 0,
                },
            ),
            X86VmExit::MsrWrite { addr, value } => sysreg::handle_write(
                vm,
                SysRegWriteExit {
                    addr: x86_msr_addr_to_ax(addr),
                    value,
                },
            ),
            X86VmExit::NestedPageFault { addr, access_flags } => handle_x86_nested_page_fault(
                vm,
                NestedPageFaultExit {
                    addr: x86_guest_phys_addr_to_ax(addr),
                    access_flags: x86_access_flags_to_ax(access_flags),
                },
            ),
            X86VmExit::ExternalInterrupt { vector } => {
                debug!("VM[{}] run VCpu[{}] get irq {vector}", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Defer(DeferredRunWork::ExternalInterrupt {
                    vector: vector as usize,
                }))
            }
            X86VmExit::PreemptionTimer => {
                Ok(BoundVcpuExit::Defer(DeferredRunWork::PreemptionTimer))
            }
            X86VmExit::InterruptEnd { vector } => {
                Ok(BoundVcpuExit::Defer(DeferredRunWork::InterruptEnd {
                    vector,
                }))
            }
            X86VmExit::Halt => {
                debug!("VM[{}] run VCpu[{}] Halt", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            X86VmExit::SystemDown => {
                warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: Some(StopReason::SystemDown),
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            X86VmExit::FailEntry {
                hardware_entry_failure_reason,
            } => {
                warn!(
                    "VM[{}] VCpu[{}] run failed with exit code {hardware_entry_failure_reason}",
                    vm.id(),
                    vcpu.id()
                );
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: None,
                    resets_vm: false,
                    exits_vcpu: false,
                }))
            }
            X86VmExit::Nothing => Ok(BoundVcpuExit::Continue),
            _ => Err(AxVmError::unsupported(
                "handle x86 VM exit",
                "unsupported VM exit reason",
            )),
        }
    }

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction> {
        exit::finish(vm, vcpu, work)
    }
}

pub(crate) struct AxvmX86HostOps;

impl X86VlapicHostOps for AxvmX86HostOps {
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

    fn register_timer(deadline_nanos: u64, callback: X86TimerCallback) -> Option<usize> {
        Some(crate::timer::register_timer(
            deadline_nanos,
            Box::new(move |deadline: Duration| callback(deadline.as_nanos() as u64)),
        ))
    }

    fn cancel_timer(token: usize) {
        crate::timer::cancel_timer(token);
    }

    fn current_vm_id() -> X86VmId {
        with_current_vcpu::<AxvmX86Vcpu, _>(|vcpu| {
            vcpu.expect("current x86 vCPU is not set").vm_id()
        })
    }

    fn current_vm_vcpu_num() -> usize {
        let vm_id = Self::current_vm_id();
        manager::with_vm(vm_id, |vm| vm.vcpu_num()).unwrap_or(0)
    }

    fn current_vm_active_vcpus() -> usize {
        manager::active_vcpu_mask(Self::current_vm_id()).unwrap_or(0)
    }

    fn active_vcpus(vm_id: X86VmId) -> Option<usize> {
        manager::active_vcpu_mask(vm_id)
    }

    fn inject_interrupt(
        vm_id: X86VmId,
        vcpu_id: X86VcpuId,
        vector: X86InterruptVector,
    ) -> X86VlapicResult {
        manager::inject_interrupt(vm_id, vcpu_id, vector as usize).map_err(ax_error_to_vlapic)
    }
}

impl X86HostOps for AxvmX86HostOps {
    fn alloc_frame() -> Option<X86HostPhysAddr> {
        default_host()
            .alloc_frame()
            .map(|addr| X86HostPhysAddr::from_usize(addr.as_usize()))
    }

    fn dealloc_frame(paddr: X86HostPhysAddr) {
        default_host().dealloc_frame(axvm_types::HostPhysAddr::from(paddr.as_usize()));
    }

    fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<X86HostPhysAddr> {
        default_host()
            .alloc_contiguous_frames(frame_count, frame_align)
            .map(|addr| X86HostPhysAddr::from_usize(addr.as_usize()))
    }

    fn dealloc_contiguous_frames(start_paddr: X86HostPhysAddr, frame_count: usize) {
        default_host().dealloc_contiguous_frames(
            axvm_types::HostPhysAddr::from(start_paddr.as_usize()),
            frame_count,
        );
    }

    fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr {
        let vaddr = default_host().phys_to_virt(axvm_types::HostPhysAddr::from(paddr.as_usize()));
        X86HostVirtAddr::from_usize(vaddr.as_usize())
    }

    fn read_guest_u8(paddr: X86GuestPhysAddr) -> X86VcpuResult<u8> {
        let vm_id = with_current_vcpu::<AxvmX86Vcpu, _>(|vcpu| vcpu.map(|vcpu| vcpu.vm_id()))
            .ok_or(X86VcpuError::BadState)?;
        let mut byte = [0u8; 1];
        let result = manager::with_vm(vm_id, |vm| {
            vm.read_from_guest(GuestPhysAddr::from(paddr.as_usize()), &mut byte)
        })
        .ok_or(X86VcpuError::BadState)?;
        result.map_err(|_| X86VcpuError::BadState)?;
        Ok(byte[0])
    }

    fn nanos_to_ticks(nanos: u64) -> u64 {
        ax_std::os::arceos::modules::ax_hal::time::nanos_to_ticks(nanos)
    }

    fn poll_host_interrupt() -> Option<u8> {
        let host_rflags = current_rflags();
        unsafe {
            asm!("sti", "nop", options(nomem, nostack));
        }
        restore_host_interrupt_flag(host_rflags);
        None
    }
}

pub(crate) struct AxvmX86Vcpu(X86Vcpu<AxvmX86HostOps>);

impl VmArchVcpuOps for AxvmX86Vcpu {
    type CreateConfig = X86VcpuCreateConfig;
    type SetupConfig = X86VcpuSetupConfig;
    type Exit = X86VmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        x86_result(X86Vcpu::new_with_config(vm_id, vcpu_id, config)).map(Self)
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
        x86_result(self.0.run())
    }

    fn bind(&mut self) -> BackendResult {
        x86_result(self.0.bind())
    }

    fn unbind(&mut self) -> BackendResult {
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
        x86_result(
            self.0
                .inject_interrupt_with_trigger(vector, x86_interrupt_is_level_triggered(trigger)),
        )
    }

    fn handle_eoi(&mut self) -> Option<u8> {
        self.0.handle_eoi()
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

const fn x86_interrupt_is_level_triggered(trigger: InterruptTriggerMode) -> bool {
    match trigger {
        InterruptTriggerMode::EdgeTriggered => false,
        InterruptTriggerMode::LevelTriggered => true,
    }
}

pub(crate) struct AxvmX86PerCpu(X86PerCpuState<AxvmX86HostOps>);

impl VmArchPerCpuOps for AxvmX86PerCpu {
    fn new(cpu_id: usize) -> BackendResult<Self> {
        x86_result(X86PerCpuState::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> BackendResult {
        x86_result(self.0.hardware_enable())
    }

    fn hardware_disable(&mut self) -> BackendResult {
        x86_result(self.0.hardware_disable())
    }
}

/// Pre-creates the canonical x86 interrupt controller and registers factories
/// that expose that same instance through the device runtime.
pub(crate) fn register_device_factories(
    vm_id: usize,
    configs: &[EmulatedDeviceConfig],
    factories: &mut DeviceFactoryRegistry,
) -> AxVmResult<Arc<X86InterruptDomain>> {
    let machine =
        crate::machine::machine_profile_for(crate::machine::MachineArchitecture::X86_64, 1);
    let expected_ioapic = unique_x86_machine_device(
        &machine.emulated_devices,
        EmulatedDeviceType::X86IoApic,
        "virtual IOAPIC",
    )?;
    let expected_pit = unique_x86_machine_device(
        &machine.emulated_devices,
        EmulatedDeviceType::X86Pit,
        "virtual PIT",
    )?;
    let ioapic_config =
        unique_x86_machine_device(configs, EmulatedDeviceType::X86IoApic, "virtual IOAPIC")?;
    let pit_config = unique_x86_machine_device(configs, EmulatedDeviceType::X86Pit, "virtual PIT")?;
    validate_device_config(
        expected_ioapic,
        ioapic_config,
        "validate x86 virtual IOAPIC machine descriptor",
    )?;
    validate_device_config(
        expected_pit,
        pit_config,
        "validate x86 virtual PIT machine descriptor",
    )?;

    let ioapic = Arc::new(axdevice::X86IoApicDevice::new(
        x86_vlapic::X86GuestPhysAddr::from_usize(ioapic_config.base_gpa),
        Some(ioapic_config.length),
    ));
    let service: Arc<dyn X86IoApicDeviceOps> = ioapic.clone();
    let domain = Arc::new(X86InterruptDomain::new(vm_id, service));
    factories.register(Arc::new(X86IoApicFactory {
        expected: ioapic_config.clone(),
        ioapic,
        domain: domain.clone(),
    }))?;
    factories.register(Arc::new(X86PitFactory {
        expected: pit_config.clone(),
    }))?;
    factories.register(Arc::new(port::HostPortPassthroughDeviceFactory))?;
    Ok(domain)
}

struct X86IoApicFactory {
    expected: EmulatedDeviceConfig,
    ioapic: Arc<axdevice::X86IoApicDevice>,
    domain: Arc<X86InterruptDomain>,
}

fn unique_x86_machine_device<'a>(
    configs: &'a [EmulatedDeviceConfig],
    device_type: EmulatedDeviceType,
    resource: &'static str,
) -> AxVmResult<&'a EmulatedDeviceConfig> {
    let mut matches = configs
        .iter()
        .filter(|config| config.emu_type == device_type);
    let config = matches.next().ok_or_else(|| {
        AxVmError::resource_unavailable("x86 machine device", std::format!("missing {resource}"))
    })?;
    if matches.next().is_some() {
        return Err(AxVmError::resource_conflict(
            "x86 machine device",
            std::format!("more than one {resource} descriptor is configured"),
        ));
    }
    Ok(config)
}

/// Adapts the IOAPIC device capability to the x86 interrupt-runtime boundary.
///
/// Guest-visible IOAPIC operations are exposed through the public interrupt
/// domain service, while host IRQ forwarding state stays in this concrete
/// VM-owned domain.
pub(super) struct X86InterruptDomain {
    wired: Arc<X86WiredState>,
    inputs: RawSpinLock<BTreeMap<usize, (InterruptTriggerMode, WiredIrqInput)>>,
    forwarding: RawSpinLock<irq::X86IoApicForwardingState>,
    forwarding_hooks: RawSpinLock<std::vec::Vec<host_irq::IrqHandle>>,
}

struct X86WiredState {
    ioapic: Arc<dyn X86IoApicDeviceOps>,
    pending: AtomicUsize,
    pending_level: AtomicUsize,
    kick: Arc<DeferredVcpuKick>,
}

/// Private key for the concrete VM-owned x86 forwarding domain.
///
/// The public `X86InterruptDomainKey` exposes only injection operations. This
/// key is intentionally architecture-private because hook ownership and
/// teardown are runtime implementation details.
pub(super) struct X86InterruptDomainRuntimeKey;

impl ServiceKey for X86InterruptDomainRuntimeKey {
    type Service = X86InterruptDomain;

    const NAME: &'static str = "x86-interrupt-domain-runtime";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

impl X86InterruptDomain {
    fn new(vm_id: usize, ioapic: Arc<dyn X86IoApicDeviceOps>) -> Self {
        Self {
            wired: Arc::new(X86WiredState {
                ioapic,
                pending: AtomicUsize::new(0),
                pending_level: AtomicUsize::new(0),
                kick: DeferredVcpuKick::new(vm_id),
            }),
            inputs: RawSpinLock::new(BTreeMap::new()),
            forwarding: RawSpinLock::new(irq::X86IoApicForwardingState::new()),
            forwarding_hooks: RawSpinLock::new(std::vec::Vec::new()),
        }
    }

    fn start_kick_worker(&self) {
        self.wired.kick.start();
    }

    fn stop_kick_worker(&self) {
        self.wired.kick.stop();
    }

    fn take_pending_wired_gsis(&self) -> (usize, usize) {
        let pending = self.wired.pending.swap(0, Ordering::AcqRel);
        let pending_level = self
            .wired
            .pending_level
            .fetch_and(!pending, Ordering::AcqRel);
        (pending, pending_level & pending)
    }

    pub(super) fn add_forwarding_hook(&self, hook: host_irq::IrqHandle) {
        self.forwarding_hooks.lock().push(hook);
    }

    pub(super) fn take_forwarding_hooks(&self) -> std::vec::Vec<host_irq::IrqHandle> {
        std::mem::take(&mut *self.forwarding_hooks.lock())
    }
}

impl X86InterruptDomainOps for X86InterruptDomain {
    fn vector_for_gsi(&self, gsi: usize) -> Option<u8> {
        self.wired.ioapic.vector_for_gsi(gsi)
    }

    fn assert_gsi(&self, gsi: usize) -> Option<x86_vlapic::IoApicInterrupt> {
        self.wired.ioapic.assert_gsi(gsi)
    }

    fn end_of_interrupt(&self, vector: u8) -> Option<x86_vlapic::IoApicEoi> {
        self.wired.ioapic.end_of_interrupt(vector)
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
                    controller: self.id(),
                    input,
                },
                operation: "open x86 IOAPIC input",
                detail: std::format!("GSI {gsi} is outside 0..{}", irq::IOAPIC_GSI_COUNT),
            });
        }
        let mut inputs = self.inputs.lock();
        if let Some((registered_trigger, registered)) = inputs.get(&gsi) {
            if *registered_trigger != trigger {
                return Err(IrqError::InvalidInput {
                    endpoint: InterruptEndpoint::Wired {
                        controller: self.id(),
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
        let sink: Arc<dyn WiredIrqSink> = self.wired.clone();
        let registered = WiredIrqInput::new(self.id(), input, trigger, sink);
        inputs.insert(gsi, (trigger, registered.clone()));
        Ok(registered)
    }
}

impl X86WiredState {
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
        self.kick
            .publish_from_irq(0)
            .map_err(|error| IrqError::Backend {
                endpoint: InterruptEndpoint::Wired {
                    controller: InterruptControllerId::new(0),
                    input,
                },
                operation: "publish x86 IOAPIC vCPU kick",
                detail: std::format!("{error}"),
            })
    }
}

impl WiredIrqSink for X86WiredState {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        if let Some(interrupt) = self.ioapic.set_gsi_level(input.value(), asserted) {
            self.publish(input, interrupt)?;
        }
        Ok(())
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        if let Some(interrupt) = self.ioapic.assert_gsi(input.value()) {
            self.publish(input, interrupt)?;
        }
        Ok(())
    }
}

impl DeviceFactory for X86IoApicFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::X86IoApic
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        validate_device_config(&self.expected, config, "build x86 virtual IOAPIC")?;
        let service: Arc<dyn X86IoApicDeviceOps> = self.ioapic.clone();
        let domain: Arc<dyn X86InterruptDomainOps> = self.domain.clone();
        let controller: Arc<dyn VirtualInterruptController> = self.domain.clone();
        let bundle =
            DeviceBundle::from_registration(DeviceRegistration::Device(self.ioapic.clone()))
                .with_service::<X86IoApicServiceKey>(service)?;
        bundle
            .with_service::<X86InterruptDomainKey>(domain)?
            .with_service::<X86InterruptDomainRuntimeKey>(self.domain.clone())?
            .with_service::<VirtualInterruptControllerKey>(controller)
    }
}

struct X86PitFactory {
    expected: EmulatedDeviceConfig,
}

impl DeviceFactory for X86PitFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::X86Pit
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        validate_device_config(&self.expected, config, "build x86 virtual PIT")?;
        let pit = Arc::new(axdevice::X86PitDevice::<AxvmX86HostOps>::new());
        let service: Arc<dyn X86PitDeviceOps> = pit.clone();
        DeviceBundle::from_registration(DeviceRegistration::Device(pit))
            .with_service::<X86PitServiceKey>(service)
    }
}

pub(crate) fn x86_apic_access_page_addr() -> AxVmResult<axvm_types::HostPhysAddr> {
    x86_result(x86_vcpu::apic_access_page_addr::<AxvmX86HostOps>())
        .map(|addr| axvm_types::HostPhysAddr::from(addr.as_usize()))
        .map_err(|error| AxVmError::vcpu("get x86 APIC access page", error))
}

pub(crate) fn x86_apic_access_page_gpa() -> AxVmResult<axvm_types::GuestPhysAddr> {
    x86_result(x86_vcpu::apic_access_page_gpa())
        .map(|addr| axvm_types::GuestPhysAddr::from(addr.as_usize()))
        .map_err(|error| AxVmError::vcpu("get x86 APIC access page", error))
}

pub(crate) fn x86_requires_apic_access_page() -> AxVmResult<bool> {
    x86_result(x86_vcpu::requires_apic_access_page())
        .map_err(|error| AxVmError::vcpu("check x86 APIC access page", error))
}

fn handle_x86_nested_page_fault(
    vm: &crate::AxVMRef,
    exit: NestedPageFaultExit,
) -> AxVmResult<BoundVcpuExit<DeferredRunWork>> {
    if vm.get_devices()?.find_mmio_dev(exit.addr).is_some() {
        warn!(
            "VM[{}] nested page fault at {:#x} maps MMIO but x86 core did not decode it",
            vm.id(),
            exit.addr.as_usize()
        );
        return Ok(BoundVcpuExit::Complete(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
            resets_vm: false,
            exits_vcpu: false,
        }));
    }

    if vm.handle_nested_page_fault(exit.addr, exit.access_flags) {
        Ok(BoundVcpuExit::Continue)
    } else {
        warn!(
            "VM[{}] unhandled x86 nested page fault at {:#x}, access={:?}",
            vm.id(),
            exit.addr.as_usize(),
            exit.access_flags
        );
        Ok(BoundVcpuExit::Complete(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
            resets_vm: false,
            exits_vcpu: false,
        }))
    }
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
    }
}

fn ax_error_to_vlapic(_err: crate::AxVmError) -> X86VlapicError {
    X86VlapicError::BadState
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

fn x86_qemu_shutdown_port(port: X86Port, width: X86AccessWidth, data: u64) -> bool {
    port.number() == QEMU_EXIT_PORT && width == X86AccessWidth::Word && data == QEMU_EXIT_MAGIC
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
    use axdevice::{DeviceRuntime, X86InterruptDomainKey, X86IoApicServiceKey, X86PitServiceKey};

    use super::*;
    fn assert_x86_exit_type<T: VmArchVcpuOps<Exit = X86VmExit>>() {}

    #[test]
    fn axvm_x86_vcpu_uses_x86_exit_type() {
        assert_x86_exit_type::<AxvmX86Vcpu>();
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

    #[test]
    fn qemu_shutdown_port_is_axvm_policy() {
        assert!(x86_qemu_shutdown_port(
            X86Port::new(QEMU_EXIT_PORT),
            X86AccessWidth::Word,
            QEMU_EXIT_MAGIC
        ));
        assert!(!x86_qemu_shutdown_port(
            X86Port::new(QEMU_EXIT_PORT),
            X86AccessWidth::Dword,
            QEMU_EXIT_MAGIC
        ));
    }

    #[test]
    fn x86_platform_devices_are_built_by_registered_factories() {
        let mut factories = DeviceFactoryRegistry::new();
        let configs = std::vec![
            EmulatedDeviceConfig {
                name: "ioapic".into(),
                base_gpa: 0xfec0_0000,
                length: 0x1000,
                emu_type: EmulatedDeviceType::X86IoApic,
                ..Default::default()
            },
            EmulatedDeviceConfig {
                name: "pit".into(),
                base_gpa: 0x40,
                length: 0x22,
                emu_type: EmulatedDeviceType::X86Pit,
                ..Default::default()
            },
        ];
        let controller = register_device_factories(1, &configs, &mut factories).unwrap();
        let context = DeviceBuildContext::new(controller.as_ref());
        let devices = DeviceRuntime::build_with_factories(&configs, &factories, &context).unwrap();

        assert_eq!(devices.devices().count(), 2);
        assert!(devices.services().require::<X86IoApicServiceKey>().is_ok());
        assert!(
            devices
                .services()
                .require::<X86InterruptDomainKey>()
                .is_ok()
        );
        assert!(devices.services().require::<X86PitServiceKey>().is_ok());
    }

    #[test]
    fn x86_pit_factory_rejects_a_modified_machine_descriptor() {
        let mut configs =
            crate::machine::machine_profile_for(crate::machine::MachineArchitecture::X86_64, 1)
                .emulated_devices;
        configs.retain(|config| config.emu_type != EmulatedDeviceType::Console);

        let mut factories = DeviceFactoryRegistry::new();
        let controller = register_device_factories(1, &configs, &mut factories).unwrap();
        let pit = configs
            .iter_mut()
            .find(|config| config.emu_type == EmulatedDeviceType::X86Pit)
            .unwrap();
        pit.base_gpa += 1;

        let context = DeviceBuildContext::new(controller.as_ref());
        let result = DeviceRuntime::build_with_factories(&configs, &factories, &context);
        assert!(matches!(
            result,
            Err(axdevice::DeviceManagerError::InvalidConfig {
                operation: "build x86 virtual PIT",
                ..
            })
        ));
    }
}
