//! AxVM x86_64 adapter.
//!
//! This module owns the AxVM/ArceOS glue for the OS-neutral `x86_vcpu` and
//! `x86_vlapic` cores.

use alloc::{boxed::Box, sync::Arc};
use core::{arch::asm, time::Duration};

use ax_errno::{AxError, AxResult};
use axvm_types::{
    AccessWidth, EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, InterruptTriggerMode,
    MappingFlags, NestedPagingConfig, Port, SysRegAddr, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps,
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
    ArchOps, BoundVcpuExit, HypercallExit, IoReadExit, IoWriteExit, LegacyDeferredRunWork,
    MmioReadExit, MmioWriteExit, NestedPageFaultExit, SysRegReadExit, SysRegWriteExit,
    VcpuCreateContext, VcpuRunAction, VcpuSetupContext,
};
use crate::{
    StopReason,
    host::{HostConsole, HostMemory, HostTime, default_host},
    manager,
    vcpu::get_current_vcpu,
};

mod npt;

const QEMU_EXIT_PORT: u16 = 0x604;
const QEMU_EXIT_MAGIC: u64 = 0x2000;
const RFLAGS_INTERRUPT_FLAG: u64 = 1 << 9;

pub(crate) struct X86_64Arch;

pub(crate) struct X86VcpuCreateState;

impl ArchOps for X86_64Arch {
    type VCpu = AxvmX86Vcpu;
    type PerCpu = AxvmX86PerCpu;
    type VcpuCreateState = X86VcpuCreateState;
    type DeferredRunWork = LegacyDeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        x86_vcpu::has_hardware_support()
    }

    fn new_vcpu_create_state(
        _vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<Self::VcpuCreateState> {
        Ok(X86VcpuCreateState)
    }

    fn build_vcpu_create_config(
        _state: &Self::VcpuCreateState,
        _ctx: VcpuCreateContext,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::CreateConfig> {
        Ok(X86VCpuCreateConfig)
    }

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::SetupConfig> {
        let mut config = X86VCpuSetupConfig {
            emulate_com1: ctx.emulates_console,
            ..Default::default()
        };
        for port in ctx.passthrough_ports {
            x86_result(config.add_passthrough_port_range(port.base, port.length))?;
        }
        Ok(config)
    }

    fn new_nested_page_table(levels: usize) -> AxResult<Self::NestedPageTable> {
        npt::NestedPageTable::new(levels)
    }

    fn before_first_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        crate::runtime::x86_irq::enable_ioapic_irq_forwarding(vm, vcpu);
    }

    fn before_vcpu_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        crate::runtime::x86_irq::drain_pending_ioapic_irqs(vm, vcpu);
        crate::runtime::x86_irq::activate_ready_ioapic_forwarding_routes(vm);
    }

    fn after_external_interrupt(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        vector: usize,
    ) {
        crate::host::arceos::dispatch_host_irq(vector);
        crate::check_timer_events();
        crate::runtime::x86_irq::inject_pending_serial_irq(vm, vcpu);
    }

    fn after_preemption_timer(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        crate::timer::check_events();
        crate::runtime::x86_irq::inject_due_pit_irq0(vm, vcpu);
        crate::runtime::x86_irq::inject_pending_serial_irq(vm, vcpu);
    }

    fn after_interrupt_end(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        vector: Option<u8>,
    ) {
        if let Some(vector) = vector {
            crate::runtime::x86_irq::inject_pending_ioapic_irq_after_eoi(vm, vcpu, vector);
        }
    }

    fn handle_halt() -> VcpuRunAction {
        VcpuRunAction::Yield
    }

    fn on_last_vcpu_exit(vm_id: usize) {
        crate::runtime::x86_irq::disable_ioapic_irq_forwarding_for_vm(vm_id);
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
            X86VmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
            X86VmExit::PortIoRead { port, width } => super::handle_io_read::<Self>(
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
                    Ok(BoundVcpuExit::Complete(VcpuRunAction::Stop(
                        StopReason::SystemDown,
                    )))
                } else {
                    super::handle_io_write(
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
            X86VmExit::MsrRead { addr } => super::handle_sys_reg_read(
                vm,
                vcpu,
                SysRegReadExit {
                    addr: x86_msr_addr_to_ax(addr),
                    reg: 0,
                },
            ),
            X86VmExit::MsrWrite { addr, value } => super::handle_sys_reg_write(
                vm,
                SysRegWriteExit {
                    addr: x86_msr_addr_to_ax(addr),
                    value,
                },
            ),
            X86VmExit::NestedPageFault { addr, access_flags } => {
                handle_x86_nested_page_fault::<Self>(
                    vm,
                    NestedPageFaultExit {
                        addr: x86_guest_phys_addr_to_ax(addr),
                        access_flags: x86_access_flags_to_ax(access_flags),
                    },
                )
            }
            X86VmExit::ExternalInterrupt { vector } => {
                debug!("VM[{}] run VCpu[{}] get irq {vector}", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Defer(
                    LegacyDeferredRunWork::ExternalInterrupt {
                        vector: vector as usize,
                    },
                ))
            }
            X86VmExit::PreemptionTimer => {
                Ok(BoundVcpuExit::Defer(LegacyDeferredRunWork::PreemptionTimer))
            }
            X86VmExit::InterruptEnd { vector } => {
                Ok(BoundVcpuExit::Defer(LegacyDeferredRunWork::InterruptEnd {
                    vector,
                }))
            }
            X86VmExit::Halt => {
                debug!("VM[{}] run VCpu[{}] Halt", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(Self::handle_halt()))
            }
            X86VmExit::SystemDown => {
                warn!("VM[{}] run VCpu[{}] SystemDown", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(VcpuRunAction::Stop(
                    StopReason::SystemDown,
                )))
            }
            X86VmExit::FailEntry {
                hardware_entry_failure_reason,
            } => {
                warn!(
                    "VM[{}] VCpu[{}] run failed with exit code {hardware_entry_failure_reason}",
                    vm.id(),
                    vcpu.id()
                );
                Ok(BoundVcpuExit::Complete(VcpuRunAction::Yield))
            }
            X86VmExit::Nothing => Ok(BoundVcpuExit::Continue),
            _ => Err(AxError::Unsupported),
        }
    }

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxResult<VcpuRunAction> {
        super::finish_legacy_deferred_run_work::<Self>(vm, vcpu, work)
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
        crate::host::arceos::monotonic_time_nanos()
    }

    fn register_timer(deadline_nanos: u64, callback: X86TimerCallback) -> Option<usize> {
        Some(default_host().register_timer(
            deadline_nanos,
            Box::new(move |deadline: Duration| callback(deadline.as_nanos() as u64)),
        ))
    }

    fn cancel_timer(token: usize) {
        default_host().cancel_timer(token);
    }

    fn write_bytes(bytes: &[u8]) {
        default_host().write_bytes(bytes);
    }

    fn read_bytes(bytes: &mut [u8]) -> usize {
        default_host().read_bytes(bytes)
    }

    fn current_vm_id() -> X86VmId {
        get_current_vcpu::<AxvmX86Vcpu>()
            .expect("current x86 vCPU is not set")
            .vm_id()
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
        let vcpu = get_current_vcpu::<AxvmX86Vcpu>().ok_or(X86VcpuError::BadState)?;
        let mut byte = [0u8; 1];
        let result = manager::with_vm(vcpu.vm_id(), |vm| {
            vm.read_from_guest(GuestPhysAddr::from(paddr.as_usize()), &mut byte)
        })
        .ok_or(X86VcpuError::BadState)?;
        result.map_err(|_| X86VcpuError::BadState)?;
        Ok(byte[0])
    }

    fn nanos_to_ticks(nanos: u64) -> u64 {
        default_host().nanos_to_ticks(nanos)
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

impl VmArchVcpuOps for AxvmX86Vcpu {
    type CreateConfig = X86VCpuCreateConfig;
    type SetupConfig = X86VCpuSetupConfig;
    type Exit = X86VmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> AxResult<Self> {
        x86_result(x86_vcpu::X86ArchVCpu::new_with_config(
            vm_id, vcpu_id, config,
        ))
        .map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        x86_result(self.0.set_entry(ax_guest_phys_addr_to_x86(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> AxResult {
        x86_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_x86(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        x86_result(self.0.setup(config))
    }

    fn run(&mut self) -> AxResult<Self::Exit> {
        x86_result(self.0.run())
    }

    fn bind(&mut self) -> AxResult {
        x86_result(self.0.bind())
    }

    fn unbind(&mut self) -> AxResult {
        x86_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        x86_result(self.0.inject_interrupt(vector))
    }

    fn inject_interrupt_with_trigger(
        &mut self,
        vector: usize,
        trigger: InterruptTriggerMode,
    ) -> AxResult {
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
    fn new(cpu_id: usize) -> AxResult<Self> {
        x86_result(x86_vcpu::X86ArchPerCpuState::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> AxResult {
        x86_result(self.0.hardware_enable())
    }

    fn hardware_disable(&mut self) -> AxResult {
        x86_result(self.0.hardware_disable())
    }
}

pub(crate) fn register_arch_device(
    config: &EmulatedDeviceConfig,
    devices: &mut axdevice::AxVmDevices,
) -> AxResult {
    match config.emu_type {
        EmulatedDeviceType::Console => {
            let serial = Arc::new(axdevice::X86SerialPortDevice::<AxvmX86HostOps>::new());
            devices.add_x86_serial_dev(serial)?;
            info!("x86 16550 serial initialized for ports 0x3f8..=0x3ff");
        }
        EmulatedDeviceType::X86IoApic => {
            let ioapic = Arc::new(axdevice::X86IoApicDevice::new(
                x86_vlapic::X86GuestPhysAddr::from_usize(config.base_gpa),
                Some(config.length),
            ));
            devices.add_x86_ioapic_dev(ioapic)?;
            info!(
                "x86 IO APIC initialized with base GPA {:#x} and length {:#x}",
                config.base_gpa, config.length
            );
        }
        EmulatedDeviceType::X86Pit => {
            let pit = Arc::new(axdevice::X86PitDevice::<AxvmX86HostOps>::new());
            devices.add_x86_pit_dev(pit)?;
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

fn handle_x86_nested_page_fault<A>(
    vm: &crate::AxVMRef,
    exit: NestedPageFaultExit,
) -> AxResult<BoundVcpuExit<A::DeferredRunWork>>
where
    A: ArchOps<DeferredRunWork = LegacyDeferredRunWork>,
{
    if vm.get_devices()?.find_mmio_dev(exit.addr).is_some() {
        warn!(
            "VM[{}] nested page fault at {:#x} maps MMIO but x86 core did not decode it",
            vm.id(),
            exit.addr.as_usize()
        );
        return Ok(BoundVcpuExit::Complete(VcpuRunAction::Yield));
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
        Ok(BoundVcpuExit::Complete(VcpuRunAction::Yield))
    }
}

fn x86_result<T>(result: X86VcpuResult<T>) -> AxResult<T> {
    result.map_err(x86_error_to_ax)
}

fn x86_error_to_ax(err: X86VcpuError) -> AxError {
    match err {
        X86VcpuError::InvalidInput => AxError::InvalidInput,
        X86VcpuError::InvalidData => AxError::InvalidData,
        X86VcpuError::Unsupported => AxError::Unsupported,
        X86VcpuError::BadState => AxError::BadState,
        X86VcpuError::NoMemory => AxError::NoMemory,
        X86VcpuError::ResourceBusy => AxError::ResourceBusy,
    }
}

fn ax_error_to_vlapic(_err: AxError) -> X86VlapicError {
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

    fn assert_x86_exit_type<T: VmArchVcpuOps<Exit = X86VmExit>>() {}

    #[test]
    fn axvm_x86_vcpu_uses_x86_exit_type() {
        assert_x86_exit_type::<AxvmX86Vcpu>();
    }

    #[test]
    fn converts_x86_vcpu_errors_to_ax_errors() {
        assert_eq!(
            x86_error_to_ax(X86VcpuError::InvalidInput),
            AxError::InvalidInput
        );
        assert_eq!(x86_error_to_ax(X86VcpuError::NoMemory), AxError::NoMemory);
        assert_eq!(
            x86_error_to_ax(X86VcpuError::ResourceBusy),
            AxError::ResourceBusy
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
