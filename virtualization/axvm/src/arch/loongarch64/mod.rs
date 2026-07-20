use alloc::boxed::Box;
use core::time::Duration;

use ax_cpu_local::CpuPin;
use ax_memory_addr::VirtAddr;
use axvm_types::{
    AccessWidth, GuestPhysAddr, MappingFlags, NestedPagingConfig, VCpuId, VMId, VmArchPerCpuOps,
    VmArchVcpuOps, VmBackendError as BackendError, VmBackendResult as BackendResult,
};
use loongarch_vcpu::{
    LoongArchAccessFlags, LoongArchAccessWidth, LoongArchGuestPhysAddr, LoongArchHostOps,
    LoongArchHostPhysAddr, LoongArchHostVirtAddr, LoongArchNestedPagingConfig, LoongArchPerCpu,
    LoongArchVCpuCreateConfig, LoongArchVCpuSetupConfig, LoongArchVcpu, LoongArchVcpuError,
    LoongArchVcpuResult, LoongArchVmExit,
};

use super::{
    ArchOps, BoundVcpuExit, CommonDeferredRunWork, HypercallExit, MmioReadExit, MmioWriteExit,
    VcpuRunAction,
};
use crate::{
    AxVmError, AxVmResult,
    host::{HostMemory, HostTime, default_host},
};

pub(crate) mod boot;
mod capabilities;
pub(crate) mod fdt;
mod idle;
pub(crate) mod irq;
mod npt;
mod vm;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};

pub(crate) struct LoongArch64Arch;

#[derive(Clone, Copy, Debug)]
pub(crate) enum LoongArchDeferredRunWork {
    Common(CommonDeferredRunWork),
    NestedPageFault {
        addr: LoongArchGuestPhysAddr,
        access_flags: LoongArchAccessFlags,
    },
    ExternalInterrupt {
        vector: usize,
    },
    Idle,
}

impl From<CommonDeferredRunWork> for LoongArchDeferredRunWork {
    fn from(work: CommonDeferredRunWork) -> Self {
        Self::Common(work)
    }
}

impl ArchOps for LoongArch64Arch {
    type VCpu = AxvmLoongArchVcpu;
    type PerCpu = AxvmLoongArchPerCpu;
    type DeferredRunWork = LoongArchDeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        loongarch_vcpu::has_hardware_support()
    }

    fn activate_guest_irq_routes(vm: &crate::AxVMRef) -> AxVmResult {
        let routes = boot::get_guest_irq_routes(vm.id());
        if routes.is_empty() {
            return Ok(());
        }

        info!(
            "Registering {} LoongArch passthrough IRQ route(s) for VM[{}]",
            routes.len(),
            vm.id()
        );
        let devices = vm.get_devices()?;
        if !devices.has_loongarch_pch_pic() {
            return Err(AxVmError::unsupported(
                "activate LoongArch passthrough IRQ routes",
                "level IRQ passthrough requires the guest PCH-PIC deassertion boundary",
            ));
        }
        for route in routes {
            irq::register_guest_irq_route(route.physical_irq, vm.id(), 0, route.guest_vector)?;
        }
        Ok(())
    }

    fn revoke_guest_irq_routes(vm: &crate::AxVMRef) -> AxVmResult {
        irq::revoke_guest_irq_routes(vm.id())
    }

    fn prepare_vcpu_irq_owner(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult<Option<crate::architecture::ops::VcpuIrqOwnerSession>> {
        irq::prepare_guest_irq_owner_session(vm, vcpu)
    }

    fn before_first_run(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        irq::activate_guest_irq_owner(vm, vcpu)
    }

    fn service_vcpu_irq_owner(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        irq::service_guest_irq_owner(vm, vcpu)
    }

    fn drain_arch_irq_publications(
        vm: &crate::AxVMRef,
        vcpu: &crate::vcpu::BoundVcpu<'_, '_, Self::VCpu>,
    ) -> AxVmResult {
        irq::drain_guest_irq_publications(vm, vcpu)
    }

    fn inject_pending_interrupt(
        vm: &crate::AxVMRef,
        vcpu: &crate::vcpu::BoundVcpu<'_, '_, Self::VCpu>,
        interrupt: crate::vm::PendingInterrupt,
    ) -> AxVmResult {
        match interrupt {
            crate::vm::PendingInterrupt::Normal(vector) => {
                trace!(
                    "Injecting queued interrupt {vector:#x} into VM[{}] VCpu[{}]",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                vcpu.inject_interrupt(vector)
            }
            crate::vm::PendingInterrupt::Triggered { vector, trigger } => {
                trace!(
                    "Injecting queued {trigger:?} interrupt {vector:#x} into LoongArch VM[{}] \
                     VCpu[{}] without trigger-specific backend state",
                    vcpu.vm_id(),
                    vcpu.id()
                );
                vcpu.inject_interrupt(vector)
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
                    return Ok(());
                };
                trace!(
                    "Injecting queued LoongArch external interrupt vector={vector:#x}, \
                     physical_irq={physical_irq:#x} into VM[{}] VCpu[{}]",
                    vm.id(),
                    vcpu.id()
                );
                vcpu.with_arch_vcpu("inject LoongArch external interrupt", |arch_vcpu| {
                    arch_vcpu.inject_external_interrupt(vector, physical_irq)
                })
                .and_then(core::convert::identity)
            }
        }
    }

    fn after_mmio_write(
        vm: &crate::AxVMRef,
        _vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) -> AxVmResult {
        drain_loongarch_pch_pic_events(vm);
        Ok(())
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
            LoongArchVmExit::Hypercall { nr, args } => {
                super::handle_hypercall(vm, vcpu, HypercallExit { nr, args })
            }
            LoongArchVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => super::handle_mmio_read::<Self>(
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
                vcpu,
                MmioWriteExit {
                    addr: loong_guest_phys_addr_to_ax(addr),
                    width: loong_access_width_to_ax(width),
                    data,
                },
            ),
            LoongArchVmExit::NestedPageFault { addr, access_flags } => Ok(BoundVcpuExit::Defer(
                LoongArchDeferredRunWork::NestedPageFault { addr, access_flags },
            )),
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
                Ok(BoundVcpuExit::Complete(VcpuRunAction {
                    waits_for_event: true,
                    stop_reason: None,
                }))
            }
            LoongArchVmExit::Nothing => Ok(BoundVcpuExit::Continue),
            _ => Err(AxVmError::unsupported(
                "handle LoongArch VM exit",
                "unsupported VM exit reason",
            )),
        }
    }

    fn finish_deferred_run_work(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        work: Self::DeferredRunWork,
    ) -> AxVmResult<VcpuRunAction> {
        match work {
            LoongArchDeferredRunWork::Common(work) => {
                return super::finish_deferred::<Self>(vm, vcpu, work);
            }
            LoongArchDeferredRunWork::NestedPageFault { addr, access_flags } => {
                return handle_loongarch_nested_page_fault(vm, vcpu, addr, access_flags);
            }
            LoongArchDeferredRunWork::ExternalInterrupt { vector } => {
                ax_std::os::arceos::modules::ax_hal::irq::handle_irq_from_task(vector);
            }
            LoongArchDeferredRunWork::Idle => idle::wait(vcpu),
        }
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
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
) -> AxVmResult<VcpuRunAction> {
    let ax_addr = loong_guest_phys_addr_to_ax(addr);
    if vm.get_devices()?.find_mmio_dev(ax_addr).is_some() {
        let Some(decoded) = vcpu.with_arch_vcpu("decode LoongArch MMIO fault", |arch_vcpu| {
            arch_vcpu.decode_mmio_fault(addr, access_flags)
        })?
        else {
            warn!(
                "VM[{}] VCpu[{}] nested page fault at {:#x} maps MMIO but cannot be decoded",
                vm.id(),
                vcpu.id(),
                ax_addr.as_usize()
            );
            return Ok(VcpuRunAction {
                waits_for_event: false,
                stop_reason: None,
            });
        };
        let work = match decoded {
            LoongArchVmExit::MmioRead {
                addr,
                width,
                reg,
                reg_width,
                signed_ext,
            } => CommonDeferredRunWork::MmioRead(MmioReadExit {
                addr: loong_guest_phys_addr_to_ax(addr),
                width: loong_access_width_to_ax(width),
                reg,
                reg_width: loong_access_width_to_ax(reg_width),
                signed_ext,
            }),
            LoongArchVmExit::MmioWrite { addr, width, data } => {
                CommonDeferredRunWork::MmioWrite(MmioWriteExit {
                    addr: loong_guest_phys_addr_to_ax(addr),
                    width: loong_access_width_to_ax(width),
                    data,
                })
            }
            _ => unreachable!("LoongArch MMIO decode returned a non-MMIO exit"),
        };
        return super::finish_deferred::<LoongArch64Arch>(vm, vcpu, work);
    }

    let ax_flags = loong_access_flags_to_ax(access_flags);
    if vm.handle_nested_page_fault(ax_addr, ax_flags) {
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
    } else {
        warn!(
            "VM[{}] VCpu[{}] unhandled nested page fault at {:#x}, access={:?}",
            vm.id(),
            vcpu.id(),
            ax_addr.as_usize(),
            ax_flags
        );
        Ok(VcpuRunAction {
            waits_for_event: false,
            stop_reason: None,
        })
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
                "LoongArch VM[{}] PCH-PIC input {} deasserted EIOINTC vector {}",
                vm.id(),
                event.source,
                event.vector
            );
            if let Err(error) = irq::complete_guest_irq_route(vm.id(), event.source) {
                warn!(
                    "failed to rearm LoongArch VM[{}] physical IRQ for PCH-PIC input {}: {error:?}",
                    vm.id(),
                    event.source
                );
            }
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
    ) -> Option<usize> {
        crate::timer::register_timer(deadline.as_nanos() as u64, callback)
    }

    fn cancel_timer(token: usize) {
        crate::timer::cancel_timer(token);
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
    fn inject_external_interrupt(&mut self, vector: usize, physical_irq: usize) -> AxVmResult {
        loongarch_result(self.0.inject_external_interrupt(vector, physical_irq))
            .map_err(|error| AxVmError::interrupt("inject LoongArch external interrupt", error))
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
    type Exit<'cpu> = LoongArchVmExit;

    fn new(vm_id: VMId, vcpu_id: VCpuId, config: Self::CreateConfig) -> BackendResult<Self> {
        loongarch_result(LoongArchVcpu::new(vm_id, vcpu_id, config)).map(Self)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> BackendResult {
        loongarch_result(self.0.set_entry(ax_guest_phys_addr_to_loong(entry)))
    }

    fn set_nested_page_table(&mut self, config: NestedPagingConfig) -> BackendResult {
        loongarch_result(
            self.0
                .set_nested_page_table(ax_nested_paging_to_loong(config)),
        )
    }

    fn setup(&mut self, config: Self::SetupConfig) -> BackendResult {
        loongarch_result(self.0.setup(config))
    }

    fn run<'cpu>(&'cpu mut self, cpu_pin: &'cpu CpuPin) -> BackendResult<Self::Exit<'cpu>> {
        loongarch_result(self.0.run(cpu_pin))
    }

    fn bind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        loongarch_result(self.0.bind(cpu_pin))
    }

    fn unbind(&mut self, cpu_pin: &CpuPin) -> BackendResult {
        loongarch_result(self.0.unbind(cpu_pin))
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
    }

    fn inject_interrupt(&mut self, vector: usize) -> BackendResult {
        loongarch_result(self.0.inject_interrupt(vector))
    }

    fn set_return_value(&mut self, val: usize) {
        self.0.set_return_value(val);
    }
}

pub(crate) struct AxvmLoongArchPerCpu(LoongArchPerCpu);

impl VmArchPerCpuOps for AxvmLoongArchPerCpu {
    fn new(cpu_id: usize) -> BackendResult<Self> {
        loongarch_result(LoongArchPerCpu::new(cpu_id)).map(Self)
    }

    fn is_enabled(&self) -> bool {
        self.0.is_enabled()
    }

    fn hardware_enable(&mut self, cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        loongarch_result(self.0.hardware_enable(cpu_pin))
    }

    fn hardware_disable(&mut self, cpu_pin: &ax_cpu_local::CpuPin) -> BackendResult {
        loongarch_result(self.0.hardware_disable(cpu_pin))
    }

    fn max_guest_page_table_levels(&self) -> usize {
        self.0.max_guest_page_table_levels()
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

    fn assert_loongarch_exit_type<T>()
    where
        for<'cpu> T: VmArchVcpuOps<Exit<'cpu> = LoongArchVmExit>,
    {
    }

    #[test]
    fn axvm_loongarch_vcpu_uses_loongarch_exit_type() {
        assert_loongarch_exit_type::<AxvmLoongArchVcpu>();
    }

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
