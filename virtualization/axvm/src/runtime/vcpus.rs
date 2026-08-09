// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use alloc::{boxed::Box, format, sync::Arc};
use core::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use crate::{
    AsVCpuTask, AxVmResult, GuestPhysAddr, StopReason, VCpuTask, VmStatus, VmVcpuState,
    arch::{ArchOps, CurrentArch, VcpuRunAction},
    ax_err_type,
    host::HostTime,
    irq::model::{PendingVcpuInterrupt, VirtualInterruptId},
    runtime::{VCpuRef, VMRef, sub_running_vm_count},
    vm::VmRuntimeHandle,
};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB
const PERIODIC_VIRQ_STACK_SIZE: usize = 0x10000;
// `vm.running()` becomes true before the guest installs its ISR. Keep the
// warm-up identical for every A/B variant so startup is excluded from samples.
const PERIODIC_VIRQ_GUEST_WARMUP: Duration = Duration::from_secs(2);

static VCPU_PARK_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static VCPU_WAKE_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
static NOTIFY_WOKE_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];
pub(crate) static LR_SKIP_COUNT: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn notify_woke_count(vcpu_id: usize) -> Option<&'static AtomicUsize> {
    NOTIFY_WOKE_COUNTS.get(vcpu_id)
}

/// Spawn the common host-side periodic injector used by both A and B.
pub(crate) fn spawn_periodic_virq_injector(
    vm: VMRef,
    config: crate::PeriodicVirqConfig,
) -> AxVmResult {
    validate_periodic_virq_config(&config)?;
    if vm.vcpu(config.vcpu_id).is_none() {
        return Err(ax_err_type!(
            NotFound,
            format!("vCPU {} not found", config.vcpu_id)
        ));
    }

    let task = crate::TaskInner::new(
        move || run_periodic_virq_injector(vm, config),
        format!("openrace-virq-injector-vcpu-{}", config.vcpu_id),
        PERIODIC_VIRQ_STACK_SIZE,
    );
    if let Some(cpu_id) = config.injector_cpu_id {
        let bits = 1usize.checked_shl(cpu_id as u32).ok_or_else(|| {
            ax_err_type!(
                InvalidInput,
                format!("injector CPU {cpu_id} is not representable")
            )
        })?;
        task.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(bits));
    }
    crate::host::task::spawn_task(task);
    Ok(())
}

fn validate_periodic_virq_config(config: &crate::PeriodicVirqConfig) -> AxVmResult {
    if config.samples == 0 {
        return Err(ax_err_type!(
            BadState,
            "periodic vIRQ samples must be non-zero"
        ));
    }
    if config.period.is_zero() {
        return Err(ax_err_type!(
            BadState,
            "periodic vIRQ period must be non-zero"
        ));
    }
    // The injector targets a vCPU with a fixed 64-bit host mask; reject larger
    // IDs explicitly instead of panicking inside CpuMask::one_shot.
    if config.vcpu_id >= 64 {
        return Err(ax_err_type!(
            InvalidInput,
            format!(
                "vCPU {} exceeds the 64-bit injector target mask",
                config.vcpu_id
            )
        ));
    }
    Ok(())
}

fn run_periodic_virq_injector(vm: VMRef, config: crate::PeriodicVirqConfig) {
    let wait_started = ax_std::time::Instant::now();
    while !vm.running() {
        if vm.stopped() || wait_started.elapsed() >= Duration::from_secs(5) {
            warn!(
                "OpenRace vIRQ injector did not observe VM[{}] running before timeout",
                vm.id()
            );
            return;
        }
        ax_std::thread::sleep(Duration::from_millis(1));
    }

    let targets = crate::CpuMask::<64>::one_shot(config.vcpu_id);
    let mut deadline = ax_std::time::Instant::now() + PERIODIC_VIRQ_GUEST_WARMUP + config.period;
    let mut failed = 0usize;
    for sequence in 0..config.samples {
        let now = ax_std::time::Instant::now();
        let remaining = deadline.duration_since(now);
        if !remaining.is_zero() {
            sleep_until(deadline);
        }
        let requested_ns = crate::host::default_host().monotonic_time().as_nanos() as u64;
        let result = vm.inject_interrupt_to_vcpu(targets, config.vector);
        let completed_ns = crate::host::default_host().monotonic_time().as_nanos() as u64;
        if result.is_err() {
            failed += 1;
        }
        info!(
            "VIRQ_INJECT sequence={} vm={} vcpu={} vector={} requested_ns={} completed_ns={} \
             status={}",
            sequence,
            vm.id(),
            config.vcpu_id,
            config.vector,
            requested_ns,
            completed_ns,
            if result.is_ok() { "ok" } else { "error" },
        );
        deadline += config.period;
        if !vm.running() {
            break;
        }
    }
    info!(
        "VIRQ_INJECT_COMPLETE vm={} vcpu={} vector={} samples={} errors={}",
        vm.id(),
        config.vcpu_id,
        config.vector,
        config.samples,
        failed,
    );
    info!(
        "E1_COUNTERS vcpu0_park={} vcpu0_wake={} vcpu1_park={} vcpu1_wake={} notify_woke0={} \
         notify_woke1={} lr_skip={}",
        VCPU_PARK_COUNTS[0].load(Ordering::Relaxed),
        VCPU_WAKE_COUNTS[0].load(Ordering::Relaxed),
        VCPU_PARK_COUNTS[1].load(Ordering::Relaxed),
        VCPU_WAKE_COUNTS[1].load(Ordering::Relaxed),
        NOTIFY_WOKE_COUNTS[0].load(Ordering::Relaxed),
        NOTIFY_WOKE_COUNTS[1].load(Ordering::Relaxed),
        LR_SKIP_COUNT.load(Ordering::Relaxed),
    );
    // Let the final interrupt reach and be timestamped by the guest before
    // the injector task exits.
    sleep_until(ax_std::time::Instant::now() + config.period);
}

/// Sleeps through the AxVM timer wheel instead of `ax_std::thread::sleep`.
///
/// The wheel is the only path that reprograms the shared physical timer on
/// AArch64; a plain host-task sleep is only serviced when a guest timer event
/// happens to fire the same IRQ. With a suspend-idle guest that arms no timer
/// of its own, `ax_std::thread::sleep` would never wake.
fn sleep_until(deadline: ax_std::time::Instant) {
    struct SleepWake {
        woke: core::sync::atomic::AtomicBool,
        wq: crate::WaitQueue,
    }
    let state = Arc::new(SleepWake {
        woke: core::sync::atomic::AtomicBool::new(false),
        wq: crate::WaitQueue::new(),
    });
    let callback_state = Arc::clone(&state);
    let remaining_ns = deadline
        .duration_since(ax_std::time::Instant::now())
        .as_nanos() as u64;
    let token = crate::timer::register_timer(
        crate::host::default_host().monotonic_time().as_nanos() as u64 + remaining_ns,
        Box::new(move |_| {
            callback_state
                .woke
                .store(true, core::sync::atomic::Ordering::Release);
            callback_state.wq.notify_all(false);
        }),
    );
    state
        .wq
        .wait_until(|| state.woke.load(core::sync::atomic::Ordering::Acquire));
    crate::timer::cancel_timer(token);
}

/// Blocks the current thread until it is explicitly woken up, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vcpu_id` - The vCPU whose wait queue is used to block the current thread.
fn wait(vm: &VMRef, vm_vcpus: &VmRuntimeHandle, vcpu_id: usize) {
    VCPU_PARK_COUNTS
        .get(vcpu_id)
        .map(|count| count.fetch_add(1, Ordering::Relaxed));
    vm_vcpus.wait_vcpu_until(vcpu_id, || {
        vm_vcpus.has_pending_vcpu_interrupt(vcpu_id)
            || !vm.running()
            || vm.suspending()
            || vm.stopping()
    });
    VCPU_WAKE_COUNTS
        .get(vcpu_id)
        .map(|count| count.fetch_add(1, Ordering::Relaxed));
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vcpu_id` - The vCPU whose wait queue is used for the wait.
/// * `condition` - A closure that returns a boolean value indicating whether the condition is met.
fn wait_for<F>(vm_vcpus: &VmRuntimeHandle, vcpu_id: usize, condition: F)
where
    F: Fn() -> bool,
{
    vm_vcpus.wait_vcpu_until(vcpu_id, condition);
}

/// Notifies the primary VCpu task associated with the specified VM to wake up and resume execution.
/// This function is used to notify the primary VCpu of a VM to start running after the VM has been booted.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus are to be notified.
pub(crate) fn notify_primary_vcpu(vm_id: usize) {
    // Generally, the primary VCpu is the first and **only** VCpu in the list.
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("VM[{vm_id}] not found while notifying primary vCPU");
        return;
    };
    if let Err(err) = vm
        .with_runtime(|runtime| Ok(runtime.clone()))
        .map(|runtime| runtime.notify_vcpu_startup(0))
    {
        warn!("VM[{vm_id}] vCPU runtime not found: {err:?}");
    }
}

/// Notifies all VCpu tasks associated with the specified VM to wake up.
/// This is useful when shutting down a VM to ensure all waiting vCPUs can check the shutdown flag.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus should be notified.
pub(crate) fn notify_all_vcpus(vm_id: usize) {
    if let Some(vm) = crate::get_vm_by_id(vm_id)
        && let Ok(runtime) = vm.with_runtime(|runtime| Ok(runtime.clone()))
    {
        runtime.notify_all();
    }
}

pub(crate) fn queue_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    // Take the runtime handle without holding the VM machine lock across the
    // wake: a parked vCPU evaluates its wait condition while holding its wait
    // queue lock and takes the machine lock inside `vm.running()`/`stopping()`,
    // so notifying under the machine lock is an ABBA deadlock (observed once
    // vCPUs actually park via PSCI CPU_SUSPEND standby).
    let runtime = vm.with_runtime(|runtime| Ok(runtime.clone()))?;
    runtime.dispatch_vcpu_interrupt(
        vcpu_id,
        PendingVcpuInterrupt {
            id: VirtualInterruptId(vector as u32),
            trigger: crate::InterruptTriggerMode::EdgeTriggered,
        },
    )
}

#[expect(
    dead_code,
    reason = "only the LoongArch IRQ backend queues physical interrupts"
)]
pub(crate) fn queue_external_interrupt(
    vm_id: usize,
    vcpu_id: usize,
    vector: usize,
    physical_irq: usize,
) -> AxVmResult {
    let vm = crate::get_vm_by_id(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] not found")))?;
    if !matches!(vm.status(), VmStatus::Running | VmStatus::Paused) {
        return Err(ax_err_type!(
            BadState,
            format!("VM[{vm_id}] is not accepting interrupts")
        ));
    }

    let cpu_id =
        vm.with_runtime(|runtime| runtime.queue_external_interrupt(vcpu_id, vector, physical_irq))?;
    vm.with_runtime(|runtime| {
        runtime.notify_all();
        Ok(())
    })?;
    crate::host::task::send_ipi(cpu_id);
    Ok(())
}

pub(crate) fn inject_pending_interrupts<A: ArchOps>(
    vm_id: usize,
    vcpu_id: usize,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
) {
    let Some(vm) = crate::get_vm_by_id(vm_id) else {
        warn!("VM[{vm_id}] not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };
    let Ok(interrupts) = vm.with_runtime(|runtime| Ok(runtime.drain_pending_interrupts(vcpu_id)))
    else {
        warn!("VM[{vm_id}] vCPU runtime not found, cannot drain VCpu[{vcpu_id}] interrupts");
        return;
    };

    for interrupt in interrupts {
        A::inject_pending_interrupt(&vm, vcpu, interrupt);
    }
}

/// Cleans up VCpu resources for a VM that is being deleted.
/// This removes the VM's entry from the global VCpu wait queue.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu resources should be cleaned up.
///
/// # Note
///
/// This should be called after all VCpu threads have exited to avoid resource leaks.
/// It will join all VCpu tasks to ensure they are fully cleaned up.
pub(crate) fn cleanup_vm_vcpus(vm_id: usize) {
    if let Some(vm) = crate::get_vm_by_id(vm_id)
        && let Err(err) = vm.with_runtime(|runtime| {
            runtime.join_all_vcpu_tasks(vm_id);
            Ok(())
        })
    {
        warn!("VM[{vm_id}] vCPU runtime cleanup skipped: {err:?}");
    }
}

/// Marks the VCpu of the specified VM as running.
fn mark_vcpu_running(vm: &VMRef) {
    let _ = vm.with_runtime(|runtime| {
        runtime.mark_vcpu_running();
        Ok(())
    });
}

#[cfg(test)]
type CpuOnStartAckLock<T> = std::sync::Mutex<T>;
#[cfg(not(test))]
type CpuOnStartAckLock<T> = ax_kspin::SpinNoIrq<T>;

#[allow(dead_code)]
pub(crate) struct CpuOnStartAck {
    inner: CpuOnStartAckLock<CpuOnStartAckInner>,
}

struct CpuOnStartAckInner {
    started: bool,
    cancelled: bool,
    result: Option<crate::AxVmResult>,
}

#[allow(dead_code)]
impl CpuOnStartAck {
    pub(crate) fn new() -> Self {
        Self {
            inner: CpuOnStartAckLock::new(CpuOnStartAckInner {
                started: false,
                cancelled: false,
                result: None,
            }),
        }
    }

    pub(crate) fn begin_startup(&self) -> bool {
        let mut inner = self.lock_inner();
        if inner.cancelled {
            false
        } else {
            inner.started = true;
            true
        }
    }

    pub(crate) fn cancel_before_startup(&self) -> bool {
        let mut inner = self.lock_inner();
        if inner.started || inner.result.is_some() {
            false
        } else {
            inner.cancelled = true;
            true
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.lock_inner().cancelled
    }

    pub(crate) fn complete(&self, result: crate::AxVmResult) {
        self.lock_inner().result = Some(result);
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.lock_inner().result.is_some()
    }

    pub(crate) fn take_result(&self) -> Option<crate::AxVmResult> {
        self.lock_inner().result.take()
    }

    #[cfg(test)]
    fn lock_inner(&self) -> impl core::ops::DerefMut<Target = CpuOnStartAckInner> + '_ {
        self.inner.lock().unwrap()
    }

    #[cfg(not(test))]
    fn lock_inner(&self) -> impl core::ops::DerefMut<Target = CpuOnStartAckInner> + '_ {
        self.inner.lock()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum VcpuOnError {
    AlreadyOn,
    OnPending,
    StartFailed,
}

/// Boot target VCpu on the specified VM.
/// This function is used to boot a secondary VCpu on a VM, setting the entry point and argument for the VCpu.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM on which the VCpu is to be booted.
/// * `vcpu_id` - The ID of the VCpu to be booted.
/// * `entry_point` - The entry point of the VCpu.
/// * `arg` - The argument to be passed to the VCpu.
#[allow(dead_code)]
pub(crate) fn vcpu_on(
    vm: VMRef,
    vcpu_id: usize,
    entry_point: GuestPhysAddr,
    arg: usize,
) -> Result<(), VcpuOnError> {
    let vcpu = vm
        .vcpu_list()
        .get(vcpu_id)
        .cloned()
        .ok_or(VcpuOnError::StartFailed)?;

    match vcpu.state() {
        VmVcpuState::Free => {}
        VmVcpuState::Starting => return Err(VcpuOnError::OnPending),
        VmVcpuState::Ready | VmVcpuState::Running => return Err(VcpuOnError::AlreadyOn),
        _ => return Err(VcpuOnError::StartFailed),
    }

    vcpu.reserve_for_cpu_on()
        .map_err(|_| VcpuOnError::OnPending)?;

    let start_result = (|| {
        let runtime = vm
            .with_runtime(|runtime| Ok(runtime.clone()))
            .map_err(|_| VcpuOnError::StartFailed)?;
        if runtime.has_vcpu_task(vcpu_id) {
            return Err(VcpuOnError::StartFailed);
        }

        vcpu.set_entry(entry_point)
            .map_err(|_| VcpuOnError::StartFailed)?;
        CurrentArch::set_vcpu_on_args(&vcpu, vcpu_id, arg);

        let ack = Arc::new(CpuOnStartAck::new());
        runtime
            .insert_cpu_on_start_ack(vcpu_id, ack.clone())
            .map_err(|_| VcpuOnError::StartFailed)?;

        runtime.prepare_vcpu_wait_queue(vcpu_id);
        let vcpu_task = crate::host::task::spawn_task(build_vcpu_task(&vm, vcpu.clone()));
        let cpu_id = vcpu
            .phys_cpu_set()
            .and_then(|mask| (mask != 0).then(|| mask.trailing_zeros() as usize))
            .unwrap_or_else(|| vcpu_task.cpu_id() as usize);
        if runtime.add_vcpu_task(vcpu_id, vcpu_task, cpu_id).is_err() {
            runtime.remove_cpu_on_start_ack(vcpu_id);
            return Err(VcpuOnError::StartFailed);
        }
        runtime.notify_all();

        runtime.wait_until(|| ack.is_complete() || !vm.running());

        if !ack.is_complete() && !vm.running() {
            if ack.cancel_before_startup() {
                runtime.notify_all();

                if let Some(task) = runtime.remove_vcpu_task(vcpu_id) {
                    let _ = task.join();
                }

                runtime.remove_cpu_on_start_ack(vcpu_id);
                return Err(VcpuOnError::StartFailed);
            }

            runtime.wait_until(|| ack.is_complete());
        }

        let result = ack.take_result().unwrap_or_else(|| {
            Err(ax_err_type!(
                BadState,
                format!("vCPU {vcpu_id} CPU_ON startup did not complete")
            ))
        });
        runtime.remove_cpu_on_start_ack(vcpu_id);

        if result.is_err() {
            runtime.remove_vcpu_task(vcpu_id);
            return Err(VcpuOnError::StartFailed);
        }

        Ok(())
    })();

    if start_result.is_err() && vcpu.state() == VmVcpuState::Starting {
        vcpu.rollback_cpu_on();
    }
    start_result
}
#[allow(dead_code)]
pub(crate) fn alloc_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> crate::AxTaskRef {
    crate::host::task::spawn_task(build_vcpu_task(vm, vcpu))
}

fn spawn_deferred_reset_task(vm_id: usize) {
    let reset_task = crate::TaskInner::new(
        move || {
            if let Err(err) = crate::runtime::reset_vm(vm_id) {
                warn!("VM[{vm_id}] deferred reset failed: {err:?}");
                crate::host::task::wait_queue_wake(&super::VMM, 1);
            }
        },
        format!("VM[{vm_id}]-reset"),
        KERNEL_STACK_SIZE,
    );
    crate::host::task::spawn_task(reset_task);
}

pub(crate) fn build_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> crate::TaskInner {
    info!("Spawning task for VM[{}] VCpu[{}]", vm.id(), vcpu.id());
    let mut vcpu_task = crate::TaskInner::new(
        vcpu_run,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        KERNEL_STACK_SIZE,
    );

    if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
        vcpu_task.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(
            vcpu_task_cpu_mask(vm.id(), vcpu.id(), phys_cpu_set),
        ));
    }

    // Use Weak reference in TaskExt to avoid keeping VM alive
    let inner = VCpuTask::new(vm, vcpu);
    *vcpu_task.task_ext_mut() = Some(crate::AxTaskExt::from_impl(inner));

    info!(
        "VCpu task {} created {:?}",
        vcpu_task.id_name(),
        vcpu_task.cpumask()
    );
    vcpu_task
}

fn vcpu_task_cpu_mask(vm_id: usize, vcpu_id: usize, requested_mask: usize) -> usize {
    let enabled_mask = crate::percpu::enabled_cpu_mask();
    if enabled_mask == 0 {
        warn!(
            "VM[{vm_id}] VCpu[{vcpu_id}] has no initialized host CPU mask; using requested mask \
             {requested_mask:#x}"
        );
        return requested_mask;
    }

    let initialized_requested_mask = requested_mask & enabled_mask;
    if initialized_requested_mask != 0 {
        if initialized_requested_mask != requested_mask {
            warn!(
                "VM[{vm_id}] VCpu[{vcpu_id}] requested host CPU mask {requested_mask:#x}, but \
                 only {initialized_requested_mask:#x} is initialized for AxVM"
            );
        }
        return initialized_requested_mask;
    }

    let fallback_mask = enabled_mask.isolate_lowest_one();
    warn!(
        "VM[{vm_id}] VCpu[{vcpu_id}] requested host CPU mask {requested_mask:#x}, but none of \
         those CPUs initialized AxVM; using initialized host CPU mask {fallback_mask:#x}"
    );
    fallback_mask
}

/// The main routine for VCpu task.
/// This function is the entry point for the VCpu tasks, which are spawned for each VCpu of a VM.
///
/// When the VCpu first starts running, it waits for the VM to be in the running state.
/// It then enters a loop where it runs the VCpu and handles the various exit reasons.
fn vcpu_run() {
    let curr = crate::host::task::current_task();

    let vm = curr.as_vcpu_task().vm();
    let vcpu = curr.as_vcpu_task().vcpu.clone();
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();
    let Ok(runtime) = vm.with_runtime(|runtime| Ok(runtime.clone())) else {
        warn!("VM[{vm_id}] vCPU runtime not found, VCpu[{vcpu_id}] exiting");
        return;
    };

    info!("VM[{}] VCpu[{}] waiting for running", vm.id(), vcpu.id());
    let cpu_on_start_ack = runtime.cpu_on_start_ack(vcpu_id);
    if cpu_on_start_ack.is_some() || !vm.running() {
        // The per-vCPU wait queue is not safe to block on before the vCPU has
        // completed its startup handshake: the run loop may hold the per-CPU
        // run queue / current-vCPU publication, and a wake can deadlock or
        // corrupt host task state (observed on dual-vCPU PSCI_CPU_ON). Use the
        // VM-wide queue for the one-shot startup barrier, matching the primary
        // vCPU boot path; steady-state WFI still uses the per-vCPU queue.
        runtime.wait_until(|| {
            vm.running()
                || cpu_on_start_ack
                    .as_ref()
                    .is_some_and(|ack| ack.is_cancelled())
        });
    }

    if let Some(ack) = &cpu_on_start_ack {
        if !ack.begin_startup() {
            ack.complete(Err(ax_err_type!(
                BadState,
                format!("vCPU {vcpu_id} CPU_ON startup was cancelled")
            )));
            runtime.notify_all();
            return;
        }

        match vcpu.bind_after_cpu_on_or_rollback() {
            Ok(()) => {
                CurrentArch::before_first_run(&vm, &vcpu);
                runtime.publish_cpu_on_start_success(ack);
                runtime.notify_all();
            }
            Err(err) => {
                ack.complete(Err(err));
                runtime.notify_all();
                runtime.remove_cpu_on_start_ack(vcpu_id);
                runtime.remove_vcpu_task(vcpu_id);
                return;
            }
        }
    } else {
        CurrentArch::before_first_run(&vm, &vcpu);
        mark_vcpu_running(&vm);
    }
    info!("VM[{}] VCpu[{}] running...", vm.id(), vcpu.id());

    loop {
        CurrentArch::before_vcpu_run(&vm, &vcpu);

        match CurrentArch::run_vcpu(&vm, &vcpu) {
            Ok(VcpuRunAction {
                exits_vcpu: true, ..
            }) => {
                if let Err(err) = vcpu.power_off_after_cpu_off() {
                    warn!("VM[{vm_id}] VCpu[{vcpu_id}] CPU_OFF cleanup failed: {err:?}");
                }
                runtime.remove_vcpu_task(vcpu_id);
                if !runtime.consume_cpu_off_reservation(vcpu_id) {
                    let _ = runtime.mark_vcpu_exiting();
                }
                break;
            }
            Ok(VcpuRunAction {
                resets_vm: true, ..
            }) => {
                if runtime.request_deferred_reset()
                    && let Err(err) = vm.stop(StopReason::Forced)
                {
                    if vm.stopping() {
                        warn!("VM[{vm_id}] reset requested while VM is already stopping: {err:?}");
                    } else {
                        let _ = runtime.take_deferred_reset_request();
                        warn!("VM[{vm_id}] failed to request deferred reset stop: {err:?}");
                        if let Err(stop_err) = vm.stop(StopReason::Fault(format!("{err:?}"))) {
                            warn!(
                                "VM[{vm_id}] shutdown after reset request failure failed: \
                                 {stop_err:?}"
                            );
                        }
                    }
                }
                notify_all_vcpus(vm_id);
            }
            Ok(VcpuRunAction {
                stop_reason: Some(reason),
                ..
            }) => {
                if let Err(err) = vm.stop(reason) {
                    warn!("VM[{vm_id}] shutdown failed: {err:?}");
                }
                notify_all_vcpus(vm_id);
            }
            Ok(VcpuRunAction {
                waits_for_event: true,
                ..
            }) => wait(&vm, &runtime, vcpu_id),
            Ok(VcpuRunAction { .. }) => {}
            Err(err) => {
                error!("VM[{vm_id}] run VCpu[{vcpu_id}] get error {err:?}");
                if let Err(err) = vm.stop(StopReason::Fault(format!("{err:?}"))) {
                    warn!("VM[{vm_id}] shutdown failed after vCPU error: {err:?}");
                }
                // Notify all vCPUs to wake up to check the shutdown flag
                notify_all_vcpus(vm_id);
            }
        }

        // Check if the VM is suspended
        if vm.suspending() {
            debug!(
                "VM[{}] VCpu[{}] is suspended, waiting for resume...",
                vm_id, vcpu_id
            );
            wait_for(&runtime, vcpu_id, || !vm.suspending());
            info!("VM[{}] VCpu[{}] resumed from suspend", vm_id, vcpu_id);
            continue;
        }

        // Check if the VM is stopping.
        if vm.stopping() {
            warn!(
                "VM[{}] VCpu[{}] stopping because of VM stopping",
                vm_id, vcpu_id
            );

            if runtime.mark_vcpu_exiting() {
                let reset_after_stop = runtime.take_deferred_reset_request();
                info!("VM[{vm_id}] VCpu[{vcpu_id}] last VCpu exiting, decreasing running VM count");

                if let Err(err) = vm.finish_stop() {
                    warn!("VM[{vm_id}] finish stop failed: {err:?}");
                }
                info!("VM[{}] state changed to Stopped", vm_id);

                CurrentArch::on_last_vcpu_exit(&vm);

                sub_running_vm_count(1);
                if reset_after_stop {
                    spawn_deferred_reset_task(vm_id);
                } else {
                    crate::host::task::wait_queue_wake(&super::VMM, 1);
                }
            }

            break;
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}

#[cfg(test)]
mod cpu_on_start_ack_tests {

    use super::*;

    #[test]
    fn cpu_on_start_ack_cancel_before_startup_blocks_late_startup() {
        let ack = CpuOnStartAck::new();

        assert!(ack.cancel_before_startup());
        assert!(ack.is_cancelled());
        assert!(!ack.begin_startup());

        ack.complete(Err(ax_err_type!(
            BadState,
            "vCPU 1 CPU_ON startup was cancelled"
        )));

        assert!(ack.is_complete());
        assert!(ack.take_result().unwrap().is_err());
    }

    #[test]
    fn out_of_range_vcpu_counter_ids_are_ignored() {
        assert!(notify_woke_count(8).is_none());
        assert!(notify_woke_count(usize::MAX).is_none());
        VCPU_PARK_COUNTS
            .get(8)
            .map(|count| count.fetch_add(1, Ordering::Relaxed));
        VCPU_WAKE_COUNTS
            .get(usize::MAX)
            .map(|count| count.fetch_add(1, Ordering::Relaxed));
    }

    #[test]
    fn periodic_injector_rejects_vcpu_beyond_64bit_mask() {
        let config = crate::PeriodicVirqConfig {
            vcpu_id: 64,
            vector: 48,
            period: Duration::from_millis(2),
            samples: 300,
            injector_cpu_id: None,
        };
        assert!(validate_periodic_virq_config(&config).is_err());

        let valid = crate::PeriodicVirqConfig {
            vcpu_id: 63,
            ..config
        };
        assert!(validate_periodic_virq_config(&valid).is_ok());
    }
}
