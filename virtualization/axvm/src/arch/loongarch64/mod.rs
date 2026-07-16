use alloc::{boxed::Box, collections::BTreeMap, format, vec::Vec};
use core::time::Duration;

use ax_kspin::SpinNoIrq as Mutex;
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

use super::{ArchOps, BoundVcpuExit, HypercallExit, MmioReadExit, MmioWriteExit, VcpuRunAction};
use crate::{
    AxVmError, AxVmResult, VmStatus, ax_err_type,
    host::{HostMemory, HostTime, default_host},
};

pub(crate) mod boot;
mod capabilities;
pub(crate) mod fdt;
#[path = "../../machine/host_acpi.rs"]
mod host_acpi;
mod idle;
mod interrupt_controller;
pub(crate) mod irq;
#[path = "../../architecture/nested_page_fault.rs"]
mod nested_page_fault;
mod npt;
#[path = "../../machine/ns16550_model.rs"]
mod ns16550_model;
mod vm;
#[path = "../../architecture/timer_scheduler.rs"]
mod vm_timer_scheduler;

pub use capabilities::{host_fdt_bootarg, host_phys_to_virt};
pub(crate) use irq::VmArchState;

pub fn current_host_platform_snapshot()
-> crate::machine::MachinePlanResult<crate::machine::HostPlatformSnapshot> {
    host_acpi::current_host_platform_snapshot()
}

pub fn standard_machine_profile()
-> crate::machine::MachinePlanResult<crate::machine::MachineProfile> {
    Ok(crate::machine::MachineProfile::new(
        crate::machine::AddressRange::new(0x1fe0_0000, 0x0010_0000)?,
        1..=255,
    )?
    .with_interrupt_controller(crate::machine::InterruptControllerProfile::LoongArch(
        crate::machine::LoongArchInterruptProfile::new(
            crate::machine::AddressRange::new(0x1000_0000, 0x1000)?,
            crate::machine::AddressRange::new(0x2ff0_0000, 0x1_0000)?,
            crate::machine::LoongArchInterruptRouting::new(
                3,
                0,
                0x20,
                0xe0,
                crate::machine::LoongArchAcpiInterruptRouting::new(0x40, 0x40, 0xc0),
            ),
        ),
    ))
    .with_loongarch_platform(crate::machine::LoongArchPlatformProfile::new(
        crate::machine::AddressRange::new(0x1e02_0000, 0x18)?,
        crate::machine::LoongArchPciProfile::new(
            crate::machine::AddressRange::new(0x2000_0000, 0x0800_0000)?,
            crate::machine::AddressRange::new(0x4000_0000, 0x4000_0000)?,
            crate::machine::AddressRange::new(0x1800_0000, 0x1_0000)?,
            16,
        ),
        crate::machine::LoongArchPowerProfile::new(
            0x100e_001e,
            0x42,
            0x100e_001c,
            0x34,
            0x100e_001c,
            0x100e_001d,
        ),
        crate::machine::LoongArchFirmwareDevicesProfile::new(
            crate::machine::AddressRange::new(0x100d_0100, 0x100)?,
            6,
            [
                crate::machine::AddressRange::new(0x1c00_0000, 0x0100_0000)?,
                crate::machine::AddressRange::new(0x1d00_0000, 0x0100_0000)?,
            ],
            4,
        ),
    )))
}

/// Returns named resources for the standard LoongArch 16550 console.
pub fn ns16550_device_requirements() -> axdevice::DeviceManagerResult<axdevice::DeviceRequirements>
{
    ns16550_model::ns16550_device_requirements(0x1000)
}

pub(crate) struct LoongArch64Arch;

#[derive(Debug, Default)]
pub(crate) struct VmArchConfig;

impl VmArchConfig {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) const fn reset_prepared_boot_state(&mut self) {}

    pub(crate) const fn validate_prepared_boot_state(
        &self,
        _interrupt_delivery: axvm_types::InterruptDelivery,
    ) -> AxVmResult {
        Ok(())
    }
}

pub(crate) struct VmRuntimeArchState {
    pending_interrupts: Mutex<BTreeMap<usize, Vec<usize>>>,
}

impl VmRuntimeArchState {
    pub(crate) fn new() -> Self {
        Self {
            pending_interrupts: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn register_vcpu(&self, vcpu_id: usize) {
        self.pending_interrupts.lock().entry(vcpu_id).or_default();
    }

    pub(crate) fn queue_interrupt(&self, vcpu_id: usize, vector: usize) {
        self.pending_interrupts
            .lock()
            .entry(vcpu_id)
            .or_default()
            .push(vector);
    }

    pub(crate) fn drain_pending_interrupts(&self, vcpu_id: usize) -> Vec<usize> {
        self.pending_interrupts
            .lock()
            .get_mut(&vcpu_id)
            .map(core::mem::take)
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LoongArchDeferredRunWork {
    ExternalInterrupt { vector: usize },
    Idle,
}

impl ArchOps for LoongArch64Arch {
    type VCpu = AxvmLoongArchVcpu;
    type PerCpu = AxvmLoongArchPerCpu;
    type DeferredRunWork = LoongArchDeferredRunWork;
    type NestedPageTable = npt::NestedPageTable<crate::HostPagingHandler>;

    fn has_hardware_support() -> bool {
        loongarch_vcpu::has_hardware_support()
    }

    fn register_platform_irq_injector() {
        irq::register_platform_irq_injector();
    }

    fn deliver_pending_controller_interrupts(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
    ) {
        deliver_pending_controller_interrupts(vm, vcpu);
    }

    fn after_mmio_write(vm: &crate::AxVMRef) {
        let result = vm.get_devices().and_then(|devices| {
            devices
                .service_loongarch_pch_pic_outputs()
                .map_err(Into::into)
        });
        if let Err(error) = result {
            warn!(
                "failed to service LoongArch VM[{}] PCH-PIC output: {error:?}",
                vm.id()
            );
        }
    }

    fn handle_vcpu_exit_bound(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: <Self::VCpu as VmArchVcpuOps>::Exit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>> {
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
                Ok(BoundVcpuExit::Complete(VcpuRunAction::wait_for_event()))
            }
            LoongArchVmExit::Nothing => Ok(BoundVcpuExit::Complete(VcpuRunAction::resume())),
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
            LoongArchDeferredRunWork::ExternalInterrupt { vector } => {
                Self::after_external_interrupt(vm, vcpu, vector);
            }
            LoongArchDeferredRunWork::Idle => idle::wait(vcpu),
        }
        Ok(VcpuRunAction::resume())
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
) -> AxVmResult<BoundVcpuExit<LoongArchDeferredRunWork>> {
    let ax_addr = loong_guest_phys_addr_to_ax(addr);
    if vm.get_devices()?.find_mmio_dev(ax_addr).is_some() {
        let Some(decoded) = vcpu.get_arch_vcpu().decode_mmio_fault(addr, access_flags) else {
            warn!(
                "VM[{}] VCpu[{}] nested page fault at {:#x} maps MMIO but cannot be decoded",
                vm.id(),
                vcpu.id(),
                ax_addr.as_usize()
            );
            return Ok(BoundVcpuExit::Complete(VcpuRunAction::resume()));
        };
        return LoongArch64Arch::handle_vcpu_exit_bound(vm, vcpu, decoded);
    }

    let ax_flags = loong_access_flags_to_ax(access_flags);
    if nested_page_fault::handle(vm, ax_addr, ax_flags) {
        Ok(BoundVcpuExit::Continue)
    } else {
        warn!(
            "VM[{}] VCpu[{}] unhandled nested page fault at {:#x}, access={:?}",
            vm.id(),
            vcpu.id(),
            ax_addr.as_usize(),
            ax_flags
        );
        Ok(BoundVcpuExit::Complete(VcpuRunAction::resume()))
    }
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
        vm_timer_scheduler::register(deadline.as_nanos() as u64, callback)
    }

    fn cancel_timer(token: usize) {
        vm_timer_scheduler::cancel(token);
    }

    fn inject_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) {
        if let Err(err) = queue_interrupt(vm_id, vcpu_id, vector) {
            warn!(
                "failed to queue LoongArch interrupt {vector:#x} for VM[{vm_id}] VCpu[{vcpu_id}]: \
                 {err:?}"
            );
        }
    }
}

fn queue_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let cpu_id = vm.with_runtime(|runtime| {
        let cpu_id = runtime.vcpu_task_cpu(vcpu_id)?;
        runtime.arch_state().queue_interrupt(vcpu_id, vector);
        Ok(cpu_id)
    })?;
    vm.with_runtime(|runtime| {
        runtime.notify_all();
        Ok(())
    })?;
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

fn deliver_pending_controller_interrupts(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmLoongArchVcpu>,
) {
    let Ok(interrupts) =
        vm.with_runtime(|runtime| Ok(runtime.arch_state().drain_pending_interrupts(vcpu.id())))
    else {
        warn!(
            "VM[{}] vCPU runtime not found while synchronizing the LoongArch interrupt controller",
            vm.id()
        );
        return;
    };
    for vector in interrupts {
        if let Err(error) = vcpu.get_arch_vcpu().deliver_controller_interrupt(vector) {
            warn!(
                "failed to deliver LoongArch controller vector {vector:#x} to VM[{}] VCpu[{}]: \
                 {error}",
                vm.id(),
                vcpu.id()
            );
        }
    }
}

pub(crate) struct AxvmLoongArchVcpu(LoongArchVcpu<AxvmLoongArchHostOps>);

impl AxvmLoongArchVcpu {
    fn deliver_controller_interrupt(&mut self, vector: usize) -> BackendResult {
        loongarch_result(self.0.inject_interrupt(vector))
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

    fn run(&mut self) -> BackendResult<Self::Exit> {
        loongarch_result(self.0.run())
    }

    fn bind(&mut self) -> BackendResult {
        loongarch_result(self.0.bind())
    }

    fn unbind(&mut self) -> BackendResult {
        loongarch_result(self.0.unbind())
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.0.set_gpr(reg, val);
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

    fn hardware_enable(&mut self) -> BackendResult {
        loongarch_result(self.0.hardware_enable())
    }

    fn hardware_disable(&mut self) -> BackendResult {
        loongarch_result(self.0.hardware_disable())
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

    fn assert_loongarch_exit_type<T: VmArchVcpuOps<Exit = LoongArchVmExit>>() {}

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
