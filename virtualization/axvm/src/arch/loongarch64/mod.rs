use alloc::boxed::Box;
use core::time::Duration;

use ax_errno::{AxError, AxResult};
use ax_memory_addr::VirtAddr;
use axvm_types::{
    AccessWidth, GuestPhysAddr, MappingFlags, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps,
};
use loongarch_vcpu::{
    LoongArchAccessFlags, LoongArchAccessWidth, LoongArchGuestPhysAddr, LoongArchHostOps,
    LoongArchHostPhysAddr, LoongArchHostVirtAddr, LoongArchIocsrStateRef,
    LoongArchNestedPagingConfig, LoongArchPerCpu, LoongArchVCpuCreateConfig,
    LoongArchVCpuSetupConfig, LoongArchVcpu, LoongArchVcpuError, LoongArchVcpuResult,
    LoongArchVmExit,
};

use super::{
    ArchOps, BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuCreateContext,
    VcpuRunAction, VcpuSetupContext,
};
use crate::host::{HostMemory, HostTime, default_host};

pub(crate) mod irq;
mod npt;

pub(crate) struct LoongArch64Arch;

#[derive(Clone, Copy, Debug)]
pub(crate) enum LoongArchDeferredRunWork {
    ExternalInterrupt { vector: usize },
    Idle,
}

impl ArchOps for LoongArch64Arch {
    type VCpu = AxvmLoongArchVcpu;
    type PerCpu = AxvmLoongArchPerCpu;
    type VcpuCreateState = LoongArchIocsrStateRef;
    type DeferredRunWork = LoongArchDeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        loongarch_vcpu::has_hardware_support()
    }

    fn new_vcpu_create_state(
        vcpu_mappings: &[(usize, Option<usize>, usize)],
    ) -> AxResult<Self::VcpuCreateState> {
        let vcpu_state_count = vcpu_mappings
            .iter()
            .map(|(vcpu_id, ..)| *vcpu_id)
            .max()
            .map_or(0, |vcpu_id| vcpu_id + 1);
        loongarch_result(loongarch_vcpu::LoongArchIocsrState::new(vcpu_state_count))
    }

    fn build_vcpu_create_config(
        state: &Self::VcpuCreateState,
        ctx: VcpuCreateContext,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::CreateConfig> {
        Ok(LoongArchVCpuCreateConfig {
            cpu_id: ctx.vcpu_id,
            dtb_addr: ctx.dtb_addr.unwrap_or_default().as_usize(),
            boot_args: [0; 3],
            boot_stack_top: 0,
            firmware_boot: ctx.firmware_boot,
            iocsr_state: state.clone(),
        })
    }

    fn build_vcpu_setup_config(
        ctx: VcpuSetupContext<'_>,
    ) -> AxResult<<Self::VCpu as VmArchVcpuOps>::SetupConfig> {
        let passthrough = ctx.interrupt_mode == axvm_types::VMInterruptMode::Passthrough;
        Ok(LoongArchVCpuSetupConfig {
            passthrough_interrupt: passthrough,
            passthrough_timer: passthrough,
            boot_args: [0; 3],
            boot_stack_top: 0,
            firmware_boot: ctx.firmware_boot,
        })
    }

    fn new_nested_page_table(levels: usize) -> AxResult<Self::NestedPageTable> {
        npt::NestedPageTable::new(levels)
    }

    fn register_platform_irq_injector() {
        irq::register_platform_irq_injector();
    }

    fn inject_pending_interrupt(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        interrupt: crate::vm::PendingInterrupt,
    ) {
        match interrupt {
            crate::vm::PendingInterrupt::Normal(vector) => {
                trace!(
                    "Injecting queued interrupt {vector:#x} into VM[{}] VCpu[{}]",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                if let Err(err) = vcpu.inject_interrupt(vector) {
                    warn!(
                        "Failed to inject queued interrupt {vector:#x} into VM[{}] VCpu[{}]: \
                         {err:?}",
                        vcpu.vm_id(),
                        vcpu.id()
                    );
                }
            }
            crate::vm::PendingInterrupt::External {
                vector,
                physical_irq,
            } => {
                let Some(vector) = loongarch_external_irq_vector(vm, vector, physical_irq) else {
                    trace!(
                        "Queued LoongArch external interrupt physical_irq={physical_irq:#x} is \
                         masked in VM[{}]",
                        vm.id()
                    );
                    return;
                };
                trace!(
                    "Injecting queued LoongArch external interrupt vector={vector:#x}, \
                     physical_irq={physical_irq:#x} into VM[{}] VCpu[{}]",
                    vm.id(),
                    vcpu.id()
                );
                if let Err(err) = vcpu
                    .get_arch_vcpu()
                    .inject_external_interrupt(vector, physical_irq)
                {
                    warn!(
                        "Failed to inject queued LoongArch external interrupt vector={vector:#x}, \
                         physical_irq={physical_irq:#x} into VM[{}] VCpu[{}]: {err:?}",
                        vm.id(),
                        vcpu.id()
                    );
                }
            }
        }
    }

    fn after_mmio_write(vm: &crate::AxVMRef) {
        drain_loongarch_pch_pic_events(vm);
    }

    fn handle_idle(_vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef<Self::VCpu>) {
        crate::check_timer_events();
        if vcpu.get_arch_vcpu().has_enabled_pending_interrupt() {
            trace!(
                "VM[{}] VCpu[{}] skips idle wait because guest has enabled pending interrupt",
                vcpu.vm_id(),
                vcpu.id()
            );
            return;
        }
        let idle_timeout = vcpu.get_arch_vcpu().idle_wait_timeout();
        trace!(
            "VM[{}] VCpu[{}] host idle wait for {idle_timeout:?}",
            vcpu.vm_id(),
            vcpu.id()
        );
        ax_std::os::arceos::modules::ax_hal::asm::set_timer_irq_enabled(true);
        ax_std::os::arceos::modules::ax_hal::asm::enable_irqs();
        ax_std::os::arceos::modules::ax_hal::time::busy_wait(idle_timeout);
        ax_std::os::arceos::modules::ax_hal::asm::disable_irqs();
        ax_std::os::arceos::modules::ax_hal::asm::set_timer_irq_enabled(false);
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxResult<BoundVcpuExit<Self::DeferredRunWork>> {
        match exit {
            LoongArchVmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
            LoongArchVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => super::handle_mmio_read(
                vm,
                vcpu,
                MmioReadExit {
                    addr: loong_guest_phys_addr_to_ax(addr),
                    width: loong_access_width_to_ax(width),
                    reg,
                    reg_width: loong_access_width_to_ax(reg_width),
                    signed_ext,
                },
            ),
            LoongArchVmExit::MmioWrite { addr, width, data } => super::handle_mmio_write::<Self>(
                vm,
                MmioWriteExit {
                    addr: loong_guest_phys_addr_to_ax(addr),
                    width: loong_access_width_to_ax(width),
                    data,
                },
            ),
            LoongArchVmExit::NestedPageFault { addr, access_flags } => {
                handle_loongarch_nested_page_fault(vm, vcpu, addr, access_flags)
            }
            LoongArchVmExit::ExternalInterrupt { vector } => {
                debug!("VM[{}] run VCpu[{}] get irq {vector}", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Defer(
                    LoongArchDeferredRunWork::ExternalInterrupt {
                        vector: vector as usize,
                    },
                ))
            }
            LoongArchVmExit::Idle => {
                trace!("VM[{}] run VCpu[{}] Idle", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Defer(LoongArchDeferredRunWork::Idle))
            }
            LoongArchVmExit::Halt => {
                debug!("VM[{}] run VCpu[{}] Halt", vm.id(), vcpu.id());
                Ok(BoundVcpuExit::Complete(Self::handle_halt()))
            }
            LoongArchVmExit::Nothing => Ok(BoundVcpuExit::Complete(VcpuRunAction::Yield)),
            _ => Err(AxError::Unsupported),
        }
    }

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxResult<VcpuRunAction> {
        match work {
            LoongArchDeferredRunWork::ExternalInterrupt { vector } => {
                Self::after_external_interrupt(vm, vcpu, vector);
            }
            LoongArchDeferredRunWork::Idle => Self::handle_idle(vm, vcpu),
        }
        Ok(VcpuRunAction::Yield)
    }

    fn clean_dcache_range(addr: VirtAddr, size: usize) {
        unsafe {
            cache_range::<DCACHE_WB>(addr, size);
            core::arch::asm!("dbar 0");
        }
    }
}

fn handle_loongarch_nested_page_fault(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>,
    addr: LoongArchGuestPhysAddr,
    access_flags: LoongArchAccessFlags,
) -> AxResult<BoundVcpuExit<LoongArchDeferredRunWork>> {
    let ax_addr = loong_guest_phys_addr_to_ax(addr);
    if vm.get_devices()?.find_mmio_dev(ax_addr).is_some() {
        let Some(decoded) = vcpu.get_arch_vcpu().decode_mmio_fault(addr, access_flags) else {
            warn!(
                "VM[{}] VCpu[{}] nested page fault at {:#x} maps MMIO but cannot be decoded",
                vm.id(),
                vcpu.id(),
                ax_addr.as_usize()
            );
            return Ok(BoundVcpuExit::Complete(VcpuRunAction::Yield));
        };
        return LoongArch64Arch::handle_vcpu_exit_bound(vm, vcpu, decoded);
    }

    let ax_flags = loong_access_flags_to_ax(access_flags);
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
        Ok(BoundVcpuExit::Complete(VcpuRunAction::Yield))
    }
}

fn loongarch_external_irq_vector(
    vm: &crate::AxVMRef,
    fallback_vector: usize,
    _physical_irq: usize,
) -> Option<usize> {
    let devices = vm.get_devices().ok()?;
    match devices.loongarch_pch_pic_assert_irq(fallback_vector) {
        Some(Some(vector)) => Some(vector),
        Some(None) => None,
        None => Some(fallback_vector),
    }
}

fn drain_loongarch_pch_pic_events(vm: &crate::AxVMRef) {
    let Ok(devices) = vm.get_devices() else {
        return;
    };
    devices.drain_loongarch_pch_pic_events(|event| {
        if !event.asserted {
            trace!(
                "LoongArch VM[{}] PCH-PIC deassert event for EIOINTC vector {}",
                vm.id(),
                event.vector
            );
            return;
        }
        if let Err(err) = crate::manager::inject_vm_vcpu_interrupt(vm.id(), 0, event.vector) {
            warn!(
                "failed to inject LoongArch VM[{}] PCH-PIC output vector {}: {err:?}",
                vm.id(),
                event.vector
            );
        }
    });
}

struct AxvmLoongArchHostOps;

impl LoongArchHostOps for AxvmLoongArchHostOps {
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
    ) -> usize {
        default_host().register_timer(deadline.as_nanos() as u64, callback)
    }

    fn cancel_timer(token: usize) {
        default_host().cancel_timer(token);
    }

    fn inject_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) {
        if let Err(err) = crate::runtime::vcpus::queue_interrupt(vm_id, vcpu_id, vector) {
            warn!(
                "failed to queue LoongArch interrupt {vector:#x} for VM[{vm_id}] VCpu[{vcpu_id}]: \
                 {err:?}"
            );
        }
    }
}

pub(crate) struct AxvmLoongArchVcpu(LoongArchVcpu<AxvmLoongArchHostOps>);

impl AxvmLoongArchVcpu {
    fn inject_external_interrupt(&mut self, vector: usize, physical_irq: usize) -> AxResult {
        loongarch_result(self.0.inject_external_interrupt(vector, physical_irq))
    }

    fn has_enabled_pending_interrupt(&self) -> bool {
        self.0.has_enabled_pending_interrupt()
    }

    fn idle_wait_timeout(&self) -> Duration {
        self.0.idle_wait_timeout()
    }

    fn decode_mmio_fault(
        &mut self,
        addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    ) -> Option<LoongArchVmExit> {
        self.0.decode_mmio_fault(addr, access_flags)
    }
}

impl VmArchVcpuOps for AxvmLoongArchVcpu {
    type CreateConfig = LoongArchVCpuCreateConfig;
    type SetupConfig = LoongArchVCpuSetupConfig;
    type Exit = LoongArchVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> AxResult<Self> {
        loongarch_result(LoongArchVcpu::new(vm_id, vcpu_id, config)).map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        loongarch_result(self.0.set_entry(ax_guest_phys_addr_to_loong(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> AxResult {
        loongarch_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_loong(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> AxResult {
        loongarch_result(self.0.setup(config))
    }

    fn run(&mut self) -> AxResult<Self::Exit> {
        loongarch_result(self.0.run())
    }

    fn bind(&mut self) -> AxResult {
        loongarch_result(self.0.bind())
    }

    fn unbind(&mut self) -> AxResult {
        loongarch_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        loongarch_result(self.0.inject_interrupt(vector))
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

pub(crate) struct AxvmLoongArchPerCpu(LoongArchPerCpu);

impl VmArchPerCpuOps for AxvmLoongArchPerCpu {
    fn new(cpu_id: usize) -> AxResult<Self> {
        loongarch_result(LoongArchPerCpu::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self) -> AxResult {
        loongarch_result(self.0.hardware_enable())
    }

    fn hardware_disable(&mut self) -> AxResult {
        loongarch_result(self.0.hardware_disable())
    }

    fn max_guest_page_table_levels(&self) -> usize {
        self.0.max_guest_page_table_levels()
    }
}

fn loongarch_result<T>(result: LoongArchVcpuResult<T>) -> AxResult<T> {
    result.map_err(loongarch_error_to_ax)
}

fn loongarch_error_to_ax(err: LoongArchVcpuError) -> AxError {
    match err {
        LoongArchVcpuError::InvalidInput => AxError::InvalidInput,
        LoongArchVcpuError::Unsupported => AxError::Unsupported,
        LoongArchVcpuError::BadState => AxError::BadState,
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

const CACHE_LINE_SIZE: usize = 64;
const DCACHE_WB: u8 = 0x19;

unsafe fn cache_range<const OP: u8>(addr: VirtAddr, size: usize) {
    if size == 0 {
        return;
    }

    let start = addr.as_usize() & !(CACHE_LINE_SIZE - 1);
    let end = addr.as_usize() + size;
    let mut current = start;

    while current < end {
        unsafe {
            core::arch::asm!("cacop {0}, {1}, 0", const OP, in(reg) current);
        }
        current += CACHE_LINE_SIZE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_loongarch_exit_type<T: VmArchVcpuOps<Exit = LoongArchVmExit>>() {}

    #[test]
    fn axvm_loongarch_vcpu_uses_loongarch_exit_type() {
        assert_loongarch_exit_type::<AxvmLoongArchVcpu>();
    }

    #[test]
    fn converts_loongarch_vcpu_errors_to_ax_errors() {
        assert_eq!(
            loongarch_error_to_ax(LoongArchVcpuError::InvalidInput),
            AxError::InvalidInput
        );
        assert_eq!(
            loongarch_error_to_ax(LoongArchVcpuError::Unsupported),
            AxError::Unsupported
        );
        assert_eq!(
            loongarch_error_to_ax(LoongArchVcpuError::BadState),
            AxError::BadState
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
