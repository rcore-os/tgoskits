use alloc::boxed::Box;
use core::time::Duration;

use ax_crate_interface::impl_interface;
use ax_errno::AxResult;
use ax_memory_addr::{PhysAddr, VirtAddr};
use loongarch_vcpu::host::LoongArchVcpuHostIf;

use super::{ArchOps, VcpuCreateContext, VcpuSetupContext};
use crate::host::{HostMemory, HostTime, default_host};

pub(crate) struct LoongArch64Arch;

impl ArchOps for LoongArch64Arch {
    type VCpu = loongarch_vcpu::LoongArchVCpu;
    type PerCpu = loongarch_vcpu::LoongArchPerCpu;
    type VcpuCreateState = loongarch_vcpu::LoongArchIocsrState;

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
        loongarch_vcpu::LoongArchIocsrState::new(vcpu_state_count)
    }

    fn build_vcpu_create_config(
        state: &Self::VcpuCreateState,
        ctx: VcpuCreateContext,
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::CreateConfig> {
        Ok(loongarch_vcpu::LoongArchVCpuCreateConfig {
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
    ) -> AxResult<<Self::VCpu as axvm_types::VmArchVcpuOps>::SetupConfig> {
        let passthrough = ctx.interrupt_mode == axvm_types::VMInterruptMode::Passthrough;
        Ok(loongarch_vcpu::LoongArchVCpuSetupConfig {
            passthrough_interrupt: passthrough,
            passthrough_timer: passthrough,
            boot_args: [0; 3],
            boot_stack_top: 0,
            firmware_boot: ctx.firmware_boot,
        })
    }

    fn register_platform_irq_injector() {
        crate::runtime::loongarch_irq::register_platform_irq_injector();
    }

    fn inject_pending_interrupt(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef,
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
                let Some(vector) = vm.loongarch_external_irq_vector(vector, physical_irq) else {
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

    fn after_mmio_write(vm: &crate::AxVM) {
        vm.drain_loongarch_pch_pic_events();
    }

    fn handle_idle(_vm: &crate::AxVMRef, vcpu: &crate::vm::AxVCpuRef) {
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

    fn clean_dcache_range(addr: VirtAddr, size: usize) {
        unsafe {
            cache_range::<DCACHE_WB>(addr, size);
            core::arch::asm!("dbar 0");
        }
    }
}

struct LoongArchVcpuHostIfImpl;

#[impl_interface]
impl LoongArchVcpuHostIf for LoongArchVcpuHostIfImpl {
    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        default_host().virt_to_phys(vaddr)
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

    fn inject_external_interrupt(vm_id: usize, vcpu_id: usize, vector: usize, physical_irq: usize) {
        if let Err(err) =
            crate::runtime::vcpus::queue_external_interrupt(vm_id, vcpu_id, vector, physical_irq)
        {
            warn!(
                "failed to queue LoongArch external interrupt vector={vector:#x}, \
                 physical_irq={physical_irq:#x} for VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}"
            );
        }
    }
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
