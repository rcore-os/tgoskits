use alloc::boxed::Box;
use core::time::Duration;

use ax_crate_interface::impl_interface;
use ax_errno::AxResult;
use ax_memory_addr::{PhysAddr, VirtAddr};
use axvm_types::{InterruptVector, VCpuId, VMId};
use x86_vcpu::host::X86VcpuHostIf;
use x86_vlapic::host::X86VlapicHostIf;

use super::{ArchOps, VcpuCreateContext, VcpuRunAction, VcpuSetupContext};
use crate::{
    host::{HostConsole, HostMemory, HostTime, default_host},
    manager,
    vcpu::get_current_vcpu,
};

mod npt;

pub(crate) struct X86_64Arch;

pub(crate) struct X86VcpuCreateState;

impl ArchOps for X86_64Arch {
    type VCpu = x86_vcpu::X86ArchVCpu;
    type PerCpu = x86_vcpu::X86ArchPerCpuState;
    type VcpuCreateState = X86VcpuCreateState;
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
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::CreateConfig> {
        Ok(x86_vcpu::X86VCpuCreateConfig)
    }

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::SetupConfig> {
        let mut config = x86_vcpu::X86VCpuSetupConfig {
            emulate_com1: ctx.emulates_console,
            ..Default::default()
        };
        for port in ctx.passthrough_ports {
            config.add_passthrough_port_range(port.base, port.length)?;
        }
        Ok(config)
    }

    fn new_nested_page_table(levels: usize) -> AxResult<Self::NestedPageTable> {
        npt::NestedPageTable::new(levels)
    }

    fn before_first_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef) {
        crate::runtime::x86_irq::enable_ioapic_irq_forwarding(vm, vcpu);
    }

    fn before_vcpu_run(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef) {
        crate::runtime::x86_irq::drain_pending_ioapic_irqs(vm, vcpu);
        crate::runtime::x86_irq::activate_ready_ioapic_forwarding_routes(vm);
    }

    fn after_external_interrupt(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef, vector: usize) {
        crate::host::arceos::dispatch_host_irq(vector);
        crate::check_timer_events();
        crate::runtime::x86_irq::inject_pending_serial_irq(vm, vcpu);
    }

    fn after_preemption_timer(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef) {
        crate::timer::check_events();
        crate::runtime::x86_irq::inject_due_pit_irq0(vm, vcpu);
        crate::runtime::x86_irq::inject_pending_serial_irq(vm, vcpu);
    }

    fn after_interrupt_end(vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef, vector: Option<u8>) {
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

    fn handle_vcpu_exit(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef,
        exit: <Self::VCpu as axvm_types::VmArchVcpuOps>::Exit,
    ) -> AxResult<VcpuRunAction> {
        super::handle_transitional_vm_exit::<Self>(vm, vcpu, exit)
    }
}

struct X86VcpuHostIfImpl;

#[impl_interface]
impl X86VcpuHostIf for X86VcpuHostIfImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        default_host().alloc_frame()
    }

    fn dealloc_frame(paddr: PhysAddr) {
        default_host().dealloc_frame(paddr);
    }

    fn alloc_contiguous_frames(frame_count: usize, frame_align: usize) -> Option<PhysAddr> {
        default_host().alloc_contiguous_frames(frame_count, frame_align)
    }

    fn dealloc_contiguous_frames(start_paddr: PhysAddr, frame_count: usize) {
        default_host().dealloc_contiguous_frames(start_paddr, frame_count);
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        default_host().phys_to_virt(paddr)
    }

    fn nanos_to_ticks(nanos: u64) -> u64 {
        default_host().nanos_to_ticks(nanos)
    }
}

struct X86VlapicHostIfImpl;

#[impl_interface]
impl X86VlapicHostIf for X86VlapicHostIfImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        default_host().alloc_frame()
    }

    fn dealloc_frame(paddr: PhysAddr) {
        default_host().dealloc_frame(paddr);
    }

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        default_host().phys_to_virt(paddr)
    }

    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        default_host().virt_to_phys(vaddr)
    }

    fn current_time() -> Duration {
        default_host().monotonic_time()
    }

    fn current_time_nanos() -> u64 {
        default_host().monotonic_time().as_nanos() as u64
    }

    fn register_timer(
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> usize {
        default_host().register_timer(deadline.as_nanos() as u64, callback)
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

    fn current_vm_id() -> VMId {
        get_current_vcpu::<x86_vcpu::X86ArchVCpu>()
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

    fn active_vcpus(vm_id: VMId) -> Option<usize> {
        manager::active_vcpu_mask(vm_id)
    }

    fn inject_interrupt(vm_id: VMId, vcpu_id: VCpuId, vector: InterruptVector) -> AxResult {
        manager::inject_interrupt(vm_id, vcpu_id, vector as usize)
    }
}
