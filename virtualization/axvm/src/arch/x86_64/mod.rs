//! AxVM x86_64 adapter.
//!
//! This module owns the AxVM/ArceOS glue for the OS-neutral `x86_vcpu` and
//! `x86_vlapic` cores.

use alloc::{boxed::Box, sync::Arc};
use core::{arch::asm, time::Duration};

use ax_cpu_local::CpuPin;
use axvm_types::{
    AccessWidth, EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, InterruptTriggerMode,
    MappingFlags, NestedPagingConfig, Port, SysRegAddr, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError as BackendError, VmBackendResult as BackendResult,
};
use x86_vcpu::{
    X86AccessFlags, X86AccessWidth, X86GuestPhysAddr, X86HostOps, X86HostPhysAddr, X86HostVirtAddr,
    X86MsrAddr, X86NestedPagingConfig, X86Port, X86VCpuCreateConfig, X86VCpuSetupConfig,
    X86VcpuError, X86VcpuResult, X86VmExit,
};
use x86_vlapic::{
    X86InterruptVector, X86TimerCallback, X86VcpuId, X86VlapicError, X86VlapicHostOps,
    X86VlapicResult, X86VmId,
};

use super::{
    ArchOps, BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction,
    current_vcpu_identity_for_task,
};
use crate::{
    AxVmError, AxVmResult, StopReason,
    host::{HostMemory, default_host},
    vcpu::current_vcpu_identity,
};

pub(crate) mod boot;
mod capabilities;
mod exit;
pub(crate) mod fdt;
mod host_irq;
pub(crate) mod irq;
mod npt;
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

impl X86_64Arch {
    fn after_external_interrupt(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<AxvmX86Vcpu>,
        vector: usize,
    ) {
        ax_std::os::arceos::modules::ax_hal::irq::handle_irq(vector);
        irq::inject_pending_serial_irq(vm, vcpu);
    }
}

impl ArchOps for X86_64Arch {
    type VCpu = AxvmX86Vcpu;
    type PerCpu = AxvmX86PerCpu;
    type DeferredRunWork = DeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        x86_vcpu::has_hardware_support()
    }

    fn before_first_run(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        irq::enable_ioapic_irq_forwarding(vm, vcpu);
        Ok(())
    }

    fn before_vcpu_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        irq::drain_pending_ioapic_irqs(vm, vcpu);
        irq::activate_ready_ioapic_forwarding_routes(vm);
    }

    fn on_last_vcpu_exit(vm_id: usize) {
        irq::disable_ioapic_irq_forwarding_for_vm(vm_id);
    }

    fn handle_vcpu_exit_bound<'cpu>(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit<'cpu>,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>
    where
        Self::VCpu: 'cpu,
    {
        match exit {
            X86VmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
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
            } => super::handle_mmio_read::<Self>(
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
                vcpu,
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
            X86VmExit::NestedPageFault { addr, access_flags } => Ok(BoundVcpuExit::Defer(
                DeferredRunWork::NestedPageFault(NestedPageFaultExit {
                    addr: x86_guest_phys_addr_to_ax(addr),
                    access_flags: x86_access_flags_to_ax(access_flags),
                }),
            )),
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
                }))
            }
            X86VmExit::SystemDown => {
                warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: false,
                    stop_reason: Some(StopReason::SystemDown),
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

fn active_vcpu_mask(vm_id: VMId) -> Option<usize> {
    crate::get_vm_by_id(vm_id).map(|vm| {
        let vcpu_num = vm.vcpu_num();
        if vcpu_num >= usize::BITS as usize {
            usize::MAX
        } else {
            (1usize << vcpu_num) - 1
        }
    })
}

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
        crate::timer::register_timer(
            deadline_nanos,
            Box::new(move |deadline: Duration| callback(deadline.as_nanos() as u64)),
        )
    }

    fn cancel_timer(token: usize) {
        crate::timer::cancel_timer(token);
    }

    fn write_bytes(bytes: &[u8]) {
        ax_std::os::arceos::modules::ax_hal::console::write_bytes(bytes);
    }

    fn read_bytes(bytes: &mut [u8]) -> usize {
        ax_std::os::arceos::modules::ax_hal::console::read_bytes(bytes)
    }

    fn current_vm_id() -> X86VmId {
        current_vcpu_identity_for_task()
            .expect("current x86 vCPU is not set")
            .into_ids()
            .0
    }

    fn current_vm_vcpu_num() -> usize {
        let vm_id = Self::current_vm_id();
        crate::get_vm_by_id(vm_id).map_or(0, |vm| vm.vcpu_num())
    }

    fn current_vm_active_vcpus() -> usize {
        active_vcpu_mask(Self::current_vm_id()).unwrap_or(0)
    }

    fn active_vcpus(vm_id: X86VmId) -> Option<usize> {
        active_vcpu_mask(vm_id)
    }

    fn inject_interrupt(
        vm_id: X86VmId,
        vcpu_id: X86VcpuId,
        vector: X86InterruptVector,
    ) -> X86VlapicResult {
        crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector as usize)
            .map_err(ax_error_to_vlapic)
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
        let identity = current_vcpu_identity().ok_or(X86VcpuError::BadState)?;
        let mut byte = [0u8; 1];
        let vm = crate::get_vm_by_id(identity.vm_id()).ok_or(X86VcpuError::BadState)?;
        let result = vm.read_from_guest(GuestPhysAddr::from(paddr.as_usize()), &mut byte);
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

pub(crate) struct AxvmX86Vcpu(x86_vcpu::X86ArchVCpu<AxvmX86HostOps>);

impl crate::vcpu::AxVCpu<AxvmX86Vcpu> {
    pub(crate) fn inject_interrupt_with_trigger(
        &self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> AxVmResult {
        self.with_arch_vcpu_access(
            crate::vcpu::BackendAccess::BoundOwnerOnly,
            "inject triggered x86 vCPU interrupt",
            |arch_vcpu| arch_vcpu.inject_interrupt_with_trigger(vector, trigger),
        )?
        .map_err(|error| {
            crate::vcpu::map_interrupt_backend_error("inject triggered x86 vCPU interrupt", error)
        })
    }
}

impl VmArchVcpuOps for AxvmX86Vcpu {
    type CreateConfig = X86VCpuCreateConfig;
    type SetupConfig = X86VCpuSetupConfig;
    type Exit<'cpu> = X86VmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        x86_result(x86_vcpu::X86ArchVCpu::new_with_config(
            vm_id, vcpu_id, config,
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

    fn run<'cpu>(&'cpu mut self, cpu_pin: &'cpu CpuPin) -> BackendResult<Self::Exit<'cpu>> {
        x86_result(self.0.run(cpu_pin))
    }

    fn bind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        x86_result(self.0.bind(cpu_pin))
    }

    fn unbind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        x86_result(self.0.unbind(cpu_pin))
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
            self.0.inject_interrupt_with_trigger(
                vector,
                trigger == InterruptTriggerMode::LevelTriggered,
            ),
        )
    }

    fn handle_eoi(&mut self) -> Option<u8> {
        self.0.handle_eoi()
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

pub(crate) struct AxvmX86PerCpu(x86_vcpu::X86ArchPerCpuState<AxvmX86HostOps>);

impl VmArchPerCpuOps for AxvmX86PerCpu {
    fn new(cpu_id: usize) -> BackendResult<Self> {
        x86_result(x86_vcpu::X86ArchPerCpuState::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self, _cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        x86_result(self.0.hardware_enable())
    }

    fn hardware_disable(&mut self, _cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        x86_result(self.0.hardware_disable())
    }
}

pub(crate) fn register_arch_device(
    config: &EmulatedDeviceConfig,
    devices: &mut axdevice::AxVmDevices,
) -> AxVmResult {
    match config.emu_type {
        EmulatedDeviceType::Console => {
            let serial = Arc::new(axdevice::X86SerialPortDevice::<AxvmX86HostOps>::new());
            devices
                .add_x86_serial_dev(serial)
                .map_err(|error| AxVmError::device("register x86 serial device", error))?;
            info!("x86 16550 serial initialized for ports 0x3f8..=0x3ff");
        }
        EmulatedDeviceType::X86IoApic => {
            let ioapic = Arc::new(axdevice::X86IoApicDevice::new(
                x86_vlapic::X86GuestPhysAddr::from_usize(config.base_gpa),
                Some(config.length),
            ));
            devices
                .add_x86_ioapic_dev(ioapic)
                .map_err(|error| AxVmError::device("register x86 I/O APIC", error))?;
            info!(
                "x86 IO APIC initialized with base GPA {:#x} and length {:#x}",
                config.base_gpa, config.length
            );
        }
        EmulatedDeviceType::X86Pit => {
            let pit = Arc::new(axdevice::X86PitDevice::<AxvmX86HostOps>::new());
            devices
                .add_x86_pit_dev(pit)
                .map_err(|error| AxVmError::device("register x86 PIT", error))?;
            info!("x86 PIT initialized for ports 0x40..=0x43 and 0x61");
        }
        _ => {}
    }
    Ok(())
}

#[cfg(feature = "vmx")]
pub(crate) fn x86_apic_access_page_addr() -> axvm_types::HostPhysAddr {
    let addr = x86_vcpu::x86_apic_access_page_addr::<AxvmX86HostOps>();
    axvm_types::HostPhysAddr::from(addr.as_usize())
}

fn handle_x86_nested_page_fault(
    vm: &crate::AxVMRef,
    exit: NestedPageFaultExit,
) -> AxVmResult<VcpuRunAction> {
    if vm.get_devices()?.find_mmio_dev(exit.addr).is_some() {
        warn!(
            "VM[{}] nested page fault at {:#x} maps MMIO but x86 core did not decode it",
            vm.id(),
            exit.addr.as_usize()
        );
        return Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        });
    }

    if vm.handle_nested_page_fault(exit.addr, exit.access_flags) {
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
    } else {
        warn!(
            "VM[{}] unhandled x86 nested page fault at {:#x}, access={:?}",
            vm.id(),
            exit.addr.as_usize(),
            exit.access_flags
        );
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
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
    use super::*;

    fn assert_x86_exit_type<T>()
    where
        for<'cpu> T: VmArchVcpuOps<Exit<'cpu> = X86VmExit>,
    {
    }

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
}
