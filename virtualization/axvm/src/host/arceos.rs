//! Default private ArceOS host adapter for AxVM.

use std::{
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use ax_memory_addr::PAGE_SIZE_4K;
use ax_std::os::arceos::{api, modules, task as runtime_task};
use axvm_types::{HostPhysAddr, HostVirtAddr};

#[cfg(any(feature = "fs", feature = "host-fs"))]
use crate::AxVmError;
#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
use crate::host::HostHardTimerAction;
#[cfg(target_arch = "x86_64")]
use crate::host::HostTimerAction;
use crate::{
    AxVmResult,
    arch::current::CurrentArch,
    architecture::ArchOps,
    host::{HostCpu, HostMemory, HostPlatform, HostTime, HostTimer},
};

/// Private default host adapter used by [`crate::AxvmRuntime`].
pub(crate) struct ArceOsHost;

const CPU_ENABLE_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const AXVM_KERNEL_STACK_SIZE: usize = 0x40000;

static ARCEOS_HOST: ArceOsHost = ArceOsHost;

pub(crate) fn arceos_host() -> &'static ArceOsHost {
    &ARCEOS_HOST
}

impl HostMemory for ArceOsHost {
    fn alloc_frame(&self) -> Option<HostPhysAddr> {
        modules::ax_alloc::global_allocator()
            .alloc_pages(1, PAGE_SIZE_4K, modules::ax_alloc::UsageKind::PageTable)
            .map(|vaddr| self.virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_frame(&self, paddr: HostPhysAddr) {
        modules::ax_alloc::global_allocator().dealloc_pages(
            self.phys_to_virt(paddr).as_usize(),
            1,
            modules::ax_alloc::UsageKind::PageTable,
        );
    }

    fn alloc_contiguous_frames(
        &self,
        num_frames: usize,
        frame_align: usize,
    ) -> Option<HostPhysAddr> {
        modules::ax_alloc::global_allocator()
            .alloc_pages(
                num_frames,
                frame_align.max(PAGE_SIZE_4K),
                modules::ax_alloc::UsageKind::Dma,
            )
            .map(|vaddr| self.virt_to_phys(vaddr.into()))
            .ok()
    }

    fn dealloc_contiguous_frames(&self, paddr: HostPhysAddr, num_frames: usize) {
        modules::ax_alloc::global_allocator().dealloc_pages(
            self.phys_to_virt(paddr).as_usize(),
            num_frames,
            modules::ax_alloc::UsageKind::Dma,
        );
    }

    fn phys_to_virt(&self, paddr: HostPhysAddr) -> HostVirtAddr {
        modules::ax_hal::mem::phys_to_virt(paddr)
    }

    fn virt_to_phys(&self, vaddr: HostVirtAddr) -> HostPhysAddr {
        modules::ax_hal::mem::virt_to_phys(vaddr)
    }
}

impl HostTime for ArceOsHost {
    fn monotonic_time(&self) -> Duration {
        modules::ax_hal::time::monotonic_time()
    }
}

impl HostTimer for ArceOsHost {
    type TimerHandle = runtime_task::KernelTimerHandle;

    fn register_timer(
        &self,
        deadline: Duration,
        callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> AxVmResult<Self::TimerHandle> {
        let deadline = runtime_task::MonotonicDeadline::from_duration(deadline);
        runtime_task::register_kernel_timer(
            deadline,
            Box::new(move |now| callback(Duration::from_nanos(now.as_nanos()))),
        )
        .map_err(|error| crate::AxVmError::host("register host timer", error))
    }

    #[cfg(target_arch = "x86_64")]
    fn register_restartable_timer(
        &self,
        deadline: Duration,
        mut callback: Box<dyn FnMut(Duration) -> HostTimerAction + Send + 'static>,
    ) -> AxVmResult<Self::TimerHandle> {
        let deadline = runtime_task::MonotonicDeadline::from_duration(deadline);
        runtime_task::register_restartable_kernel_timer(
            deadline,
            Box::new(
                move |now| match callback(Duration::from_nanos(now.as_nanos())) {
                    HostTimerAction::Complete => runtime_task::KernelTimerAction::Complete,
                    HostTimerAction::Rearm(deadline) => runtime_task::KernelTimerAction::Rearm(
                        runtime_task::MonotonicDeadline::from_duration(deadline),
                    ),
                },
            ),
        )
        .map_err(|error| crate::AxVmError::host("register restartable host timer", error))
    }

    #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
    unsafe fn register_hard_restartable_timer(
        &self,
        deadline: Duration,
        mut callback: Box<dyn FnMut(Duration) -> HostHardTimerAction + Send + 'static>,
    ) -> AxVmResult<Self::TimerHandle> {
        let deadline = runtime_task::MonotonicDeadline::from_duration(deadline);
        let callback = unsafe {
            // SAFETY: the caller owns the callback's hard-IRQ proof. This
            // adapter changes only timestamp and action representations.
            runtime_task::HardKernelTimerCallback::new(Box::new(move |now| {
                match callback(Duration::from_nanos(now.as_nanos())) {
                    HostHardTimerAction::Complete => runtime_task::HardKernelTimerAction::Complete,
                    #[cfg(target_arch = "aarch64")]
                    HostHardTimerAction::Disarm => runtime_task::HardKernelTimerAction::Disarm,
                    HostHardTimerAction::Rearm(deadline) => {
                        runtime_task::HardKernelTimerAction::Rearm(
                            runtime_task::MonotonicDeadline::from_duration(deadline),
                        )
                    }
                }
            }))
        };
        runtime_task::register_hard_restartable_kernel_timer(deadline, callback)
            .map_err(|error| crate::AxVmError::host("register hard host timer", error))
    }

    #[cfg(target_arch = "aarch64")]
    fn arm_hard_timer(&self, handle: Self::TimerHandle, deadline: Duration) -> AxVmResult {
        let deadline = runtime_task::MonotonicDeadline::from_duration(deadline);
        runtime_task::arm_hard_kernel_timer(handle, deadline)
            .map_err(|error| crate::AxVmError::host("arm hard host timer", error))
    }

    #[cfg(target_arch = "aarch64")]
    fn disarm_hard_timer(&self, handle: Self::TimerHandle) -> AxVmResult {
        runtime_task::disarm_hard_kernel_timer(handle)
            .map_err(|error| crate::AxVmError::host("disarm hard host timer", error))
    }

    fn cancel_timer(&self, handle: Self::TimerHandle) -> AxVmResult<bool> {
        runtime_task::cancel_kernel_timer(handle)
            .map(|outcome| matches!(outcome, runtime_task::KernelTimerCancelOutcome::Cancelled))
            .map_err(|error| crate::AxVmError::host("cancel host timer", error))
    }
}

/// Returns the platform IRQ reserved for the physical host console.
pub(crate) fn host_console_irq() -> Option<modules::ax_hal::irq::IrqId> {
    modules::ax_hal::console::irq_num()
}

pub(crate) fn dispatch_host_irq(vector: usize) {
    modules::ax_hal::irq::handle_irq(vector);
}

impl HostCpu for ArceOsHost {
    type CpuMask = api::task::AxCpuMask;

    fn cpu_count(&self) -> usize {
        modules::ax_hal::cpu_num()
    }

    fn this_cpu_id(&self) -> usize {
        modules::ax_hal::percpu::this_cpu_id()
    }
}

pub(crate) type ArceOsThreadHandle = runtime_task::ThreadHandle;
#[cfg(target_arch = "aarch64")]
pub(crate) type ArceOsThreadWakeHandle = runtime_task::ThreadWakeHandle;
pub(crate) type ArceOsWaitQueue = runtime_task::WaitQueue;
#[cfg(target_arch = "aarch64")]
pub(crate) type ArceOsIrqError = modules::ax_hal::irq::IrqError;
pub(crate) type ArceOsWaitQueueHandle = api::task::AxWaitQueueHandle;
pub(crate) use runtime_task::{
    CpuId as ArceOsCpuId, CpuSet as ArceOsCpuSet, MonotonicDeadline as ArceOsMonotonicDeadline,
    SchedulePolicy as ArceOsSchedulePolicy, SwitchReason as ArceOsSwitchReason,
    TaskError as ArceOsTaskError, ThreadExtension as ArceOsThreadExtension,
    ThreadExtensionOps as ArceOsThreadExtensionOps, ThreadId as ArceOsThreadId,
};

/// Hard-IRQ-safe event consumed by one fixed ArceOS service thread.
pub(crate) struct ArceOsIrqNotification {
    event: runtime_task::IrqWaitCell,
    waiter: OnceLock<ArceOsIrqWaiter>,
}

struct ArceOsIrqWaiter {
    owner: ArceOsThreadId,
    registration: runtime_task::IrqWaitRegistration,
    park: runtime_task::WaitQueue,
}

impl ArceOsIrqNotification {
    pub(crate) const fn new() -> Self {
        Self {
            event: runtime_task::IrqWaitCell::new(),
            waiter: OnceLock::new(),
        }
    }

    pub(crate) fn notify(&self) {
        let _result = self.event.notify();
    }

    pub(crate) fn wait(&self) {
        let _timed_out = self.wait_until(None);
    }

    /// Waits for one event or an optional absolute monotonic deadline.
    ///
    /// Returns `true` only when the task deadline won the wake race.
    pub(crate) fn wait_until(&self, deadline: Option<ArceOsMonotonicDeadline>) -> bool {
        let current = current_thread();
        let waiter = self.waiter.get_or_init(|| ArceOsIrqWaiter {
            owner: current.id(),
            registration: runtime_task::IrqWaitRegistration::new(current.wake_handle()),
            park: runtime_task::WaitQueue::new(),
        });
        assert_eq!(
            waiter.owner,
            current.id(),
            "one AxVM IRQ notification must be consumed by one fixed service thread"
        );

        match self.event.register(&waiter.registration) {
            runtime_task::IrqRegisterResult::ConsumedPending => false,
            runtime_task::IrqRegisterResult::Registered(token)
            | runtime_task::IrqRegisterResult::NotificationInFlight(token) => {
                let timed_out = match deadline {
                    Some(deadline) => waiter
                        .park
                        .wait_until_deadline(deadline, || !token.is_attached()),
                    None => {
                        waiter.park.wait_until(|| !token.is_attached());
                        false
                    }
                };
                runtime_task::quiesce_irq_wait(token).unwrap_or_else(|error| {
                    panic!("AxVM IRQ notification could not quiesce: {error}")
                });
                timed_out
            }
            runtime_task::IrqRegisterResult::Occupied => {
                panic!("AxVM IRQ notification waiter was registered concurrently")
            }
        }
    }
}

pub(crate) fn current_thread() -> ArceOsThreadHandle {
    runtime_task::current_thread_handle()
        .unwrap_or_else(|error| panic!("AxVM requires a current scheduler thread: {error}"))
}

pub(crate) unsafe fn spawn_thread_with_extension_and_affinity<F>(
    entry: F,
    name: std::string::String,
    stack_size: usize,
    extension: Option<ArceOsThreadExtension>,
    affinity: Option<ArceOsCpuSet>,
) -> Result<ArceOsThreadHandle, ArceOsTaskError>
where
    F: FnOnce() + Send + 'static,
{
    // SAFETY: the caller transfers unique ownership of `extension`; this
    // adapter forwards it exactly once to the ArceOS runtime.
    unsafe {
        runtime_task::spawn_raw_with_extension_and_affinity(
            entry, name, stack_size, extension, affinity,
        )
    }
}

pub(crate) fn join_thread(thread: ArceOsThreadHandle) -> Result<i32, ArceOsTaskError> {
    runtime_task::join_thread(thread)
}

pub(crate) fn thread_extension(
    thread: &ArceOsThreadHandle,
) -> Result<Option<runtime_task::ThreadOsExtensionBorrow<'_>>, ArceOsTaskError> {
    runtime_task::thread_os_extension(thread)
}

pub(crate) fn cpu_set_from_raw_bits(bits: usize) -> ArceOsCpuSet {
    let cpu_count = modules::ax_hal::cpu_num();
    let mut affinity = ArceOsCpuSet::empty(cpu_count);
    for cpu_id in 0..cpu_count.min(usize::BITS as usize) {
        if bits & (1usize << cpu_id) != 0 {
            assert!(affinity.insert(ArceOsCpuId::new(cpu_id as u32)));
        }
    }
    affinity
}

pub(crate) fn cpu_set_one(cpu_id: usize) -> ArceOsCpuSet {
    let mut affinity = ArceOsCpuSet::empty(modules::ax_hal::cpu_num());
    assert!(
        affinity.insert(ArceOsCpuId::new(cpu_id as u32)),
        "AxVM task CPU {cpu_id} is outside the runtime topology"
    );
    affinity
}

pub(crate) fn thread_cpu_id(thread: &ArceOsThreadHandle) -> Option<usize> {
    thread.assigned_cpu().map(|cpu| cpu.as_usize())
}

pub(crate) fn yield_now() {
    thread::yield_now();
}

pub(crate) fn wait_queue_wait_until(
    queue: &api::task::AxWaitQueueHandle,
    condition: impl Fn() -> bool,
) {
    api::task::ax_wait_queue_wait_until(queue, condition, None);
}

pub(crate) fn wait_queue_wake(queue: &api::task::AxWaitQueueHandle, count: u32) {
    api::task::ax_wait_queue_wake(queue, count);
}

pub(crate) fn send_ipi(cpu_id: usize) {
    if modules::ax_hal::percpu::this_cpu_id() == cpu_id {
        return;
    }
    runtime_task::notify_cpu(cpu_id)
        .unwrap_or_else(|err| panic!("failed to deliver AxVM IPI to CPU {cpu_id}: {err:?}"));
}

#[cfg(target_arch = "aarch64")]
pub(crate) fn run_on_cpu_sync(
    cpu_id: usize,
    f: unsafe fn(*mut ()),
    arg: *mut (),
) -> Result<(), ArceOsIrqError> {
    // SAFETY: the caller guarantees that `arg` stays valid until the target CPU
    // has executed `f`; `ax_hal` provides the synchronous completion boundary.
    unsafe { modules::ax_hal::irq::run_on_cpu_sync(modules::ax_hal::irq::CpuId(cpu_id), f, arg) }
}

fn send_ipi_to_all_except_current(cpu_num: usize) {
    if cpu_num <= 1 {
        return;
    }
    let cpu_id = modules::ax_hal::percpu::this_cpu_id();
    for target_cpu in 0..cpu_num {
        if target_cpu == cpu_id {
            continue;
        }
        modules::ax_hal::irq::send_ipi(
            modules::ax_hal::irq::ipi_irq(),
            modules::ax_hal::irq::IpiTarget::Cpu(modules::ax_hal::irq::CpuId(target_cpu)),
        )
        .unwrap_or_else(|err| {
            panic!("failed to deliver AxVM broadcast IPI to CPU {target_cpu}: {err:?}")
        });
    }
}

#[cfg(any(feature = "fs", feature = "host-fs"))]
pub fn shutdown_host_filesystems() -> AxVmResult {
    modules::ax_fs_ng::shutdown_filesystems()
        .map_err(|error| AxVmError::host("shut down host filesystems", error))?;
    let released = modules::ax_fs_ng::release_block_irqs_for_passthrough();
    if released != 0 {
        info!("Released {released} host filesystem block IRQ registration(s) during shutdown");
    }
    Ok(())
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
pub(crate) fn register_qemu_block_passthrough_irq(vm: &crate::AxVMRef) -> AxVmResult {
    let (_, _, _, guest_gsi) = crate::boot::x86_qemu_passthrough_block_intx();
    let info = qemu_block_passthrough_pci_info();

    let route = match ax_driver::pci::resolve_intx_binding(info) {
        Ok(Some(binding)) => {
            let trigger = intx_forwarding_trigger(&binding);
            resolve_binding_irq(binding).map(|host_irq| (host_irq, trigger))
        }
        Ok(None) => {
            warn!("x86 QEMU block passthrough PCI INTx route was not found for {info:?}");
            return Ok(());
        }
        Err(error) => {
            warn!("failed to resolve x86 QEMU block passthrough PCI INTx route: {error:?}");
            return Ok(());
        }
    };

    match route {
        Ok((host_irq, trigger)) => {
            crate::arch::current::register_host_irq_forwarding_route_with_trigger(
                vm, guest_gsi, host_irq, trigger,
            )?;
            crate::arch::current::register_host_irq_forwarding_activator(
                vm,
                guest_gsi,
                unmask_qemu_block_passthrough_intx,
            )?;
            info!(
                "Registered x86 QEMU block passthrough PCI INTx forwarding route: guest GSI \
                 {guest_gsi} <- host IRQ {host_irq:?}, trigger {trigger:?}"
            );
        }
        Err(error) => {
            warn!(
                "failed to resolve x86 QEMU block passthrough IRQ source into host IRQ: {error:?}"
            );
        }
    }
    Ok(())
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
pub(crate) fn prepare_qemu_block_passthrough_device() {
    let info = qemu_block_passthrough_pci_info();
    match ax_driver::pci::prepare_intx_passthrough(info) {
        Ok(()) => info!("Prepared x86 QEMU block PCI INTx passthrough device {info:?}"),
        Err(error) => {
            warn!("failed to prepare x86 QEMU block PCI INTx passthrough device: {error:?}");
        }
    }
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
fn unmask_qemu_block_passthrough_intx() {
    let info = qemu_block_passthrough_pci_info();
    match ax_driver::pci::unmask_intx_passthrough(info) {
        Ok(()) => info!("Unmasked x86 QEMU block PCI INTx passthrough device {info:?}"),
        Err(error) => {
            warn!("failed to unmask x86 QEMU block PCI INTx passthrough device: {error:?}");
        }
    }
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
fn qemu_block_passthrough_pci_info() -> ax_driver::probe::pci::PciInfo {
    use ax_driver::probe::pci::{PciAddress, PciInfo, PciIntxRoute};

    let (device, function, pin, _) = crate::boot::x86_qemu_passthrough_block_intx();
    PciInfo {
        address: PciAddress::new(0, 0, device, function),
        interrupt_pin: pin,
        interrupt_line: 0,
        dma_coherent: true,
        intx_route: Some(PciIntxRoute {
            root_device: device,
            root_function: function,
            root_pin: pin,
        }),
    }
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
fn resolve_binding_irq(
    binding: ax_driver::BindingIrq,
) -> Result<modules::ax_hal::irq::IrqId, modules::ax_hal::irq::IrqError> {
    use modules::ax_hal::irq;

    if let Some(irq) = binding.irq_id() {
        return Ok(irq);
    }
    let Some(source) = binding.as_irq_source() else {
        return Err(irq::IrqError::Unsupported);
    };
    irq::resolve_irq_source(source)
}

#[cfg(all(feature = "host-fs", target_arch = "x86_64"))]
fn intx_forwarding_trigger(binding: &ax_driver::BindingIrq) -> crate::InterruptTriggerMode {
    match binding {
        ax_driver::BindingIrq::Source(ax_driver::BindingIrqSource::AcpiGsiRoute(route)) => {
            match route.trigger {
                modules::ax_hal::irq::AcpiIrqTrigger::Edge => {
                    crate::InterruptTriggerMode::EdgeTriggered
                }
                modules::ax_hal::irq::AcpiIrqTrigger::Level => {
                    crate::InterruptTriggerMode::LevelTriggered
                }
            }
        }
        _ => crate::InterruptTriggerMode::LevelTriggered,
    }
}

impl HostPlatform for ArceOsHost {
    fn has_hardware_support(&self) -> bool {
        CurrentArch::has_hardware_support()
    }

    fn enable_virtualization_on_current_cpu(&self) -> AxVmResult {
        crate::percpu::init_current_cpu()?;
        crate::percpu::enable_current_cpu()?;
        crate::percpu::mark_cpu_enabled(self.this_cpu_id());
        Ok(())
    }

    fn enable_virtualization_on_all_cpus(&self) -> AxVmResult {
        static CORES: AtomicUsize = AtomicUsize::new(0);

        info!("Enabling hardware virtualization support on all cores...");
        CORES.store(0, Ordering::Release);
        crate::percpu::reset_enabled_cpu_mask();

        let cpu_count = self.cpu_count();
        let current_cpu = self.this_cpu_id();
        info!("Core {current_cpu} is initializing hardware virtualization support...");
        self.enable_virtualization_on_current_cpu()?;
        info!("Hardware virtualization support enabled on core {current_cpu}");
        CORES.store(1, Ordering::Release);

        for cpu_id in 0..cpu_count {
            if cpu_id == current_cpu {
                continue;
            }
            let affinity = cpu_set_one(cpu_id);
            // SAFETY: no OS extension is transferred and the affinity is
            // validated against the current runtime topology above.
            let _task = unsafe {
                spawn_thread_with_extension_and_affinity(
                    move || {
                        let host = arceos_host();
                        info!("Core {cpu_id} is initializing hardware virtualization support...");
                        host.enable_virtualization_on_current_cpu()
                            .expect("failed to enable hardware virtualization");
                        info!("Hardware virtualization support enabled on core {cpu_id}");
                        let _ = CORES.fetch_add(1, Ordering::Release);
                    },
                    std::format!("axvm-hv-init-{cpu_id}"),
                    AXVM_KERNEL_STACK_SIZE,
                    None,
                    Some(affinity),
                )
            }
            .unwrap_or_else(|error| {
                panic!("failed to spawn AxVM CPU {cpu_id} initialization task: {error}")
            });
            if cpu_id != self.this_cpu_id() {
                send_ipi(cpu_id);
            }
        }

        info!("Waiting for all cores to enable hardware virtualization...");
        let start = self.monotonic_time();
        let mut wait_rounds = 0usize;
        while CORES.load(Ordering::Acquire) != cpu_count {
            thread::yield_now();
            wait_rounds = wait_rounds.wrapping_add(1);
            if wait_rounds.is_multiple_of(256) {
                send_ipi_to_all_except_current(cpu_count);
            }
            if self.monotonic_time().saturating_sub(start) >= CPU_ENABLE_WAIT_TIMEOUT {
                break;
            }
        }
        crate::arch::current::register_platform_irq_injector();
        let enabled_count = CORES.load(Ordering::Acquire);
        if enabled_count == cpu_count {
            info!("All cores have enabled hardware virtualization support.");
        } else {
            warn!(
                "Only {enabled_count}/{cpu_count} cores enabled hardware virtualization before \
                 timeout; continuing with host CPU mask {:#x}",
                crate::percpu::enabled_cpu_mask()
            );
        }
        Ok(())
    }
}
