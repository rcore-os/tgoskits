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

use alloc::{collections::BTreeMap, format, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::{AxResult, ax_err_type};
use ax_kspin::SpinNoIrq as Mutex;

use crate::{
    AsVCpuTask, AxVCpuExitReason, CpuMask, GuestPhysAddr, VCpuState, VCpuTask,
    runtime::{VCpuRef, VMRef, sub_running_vm_count},
};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

/// A global map that holds the vCPU task state for each VM.
static VM_VCPU_TASKS: Mutex<BTreeMap<usize, Arc<VMVCpus>>> = Mutex::new(BTreeMap::new());

fn get_vm_vcpus(vm_id: usize) -> Option<Arc<VMVCpus>> {
    VM_VCPU_TASKS.lock().get(&vm_id).cloned()
}

/// A structure representing the VCpus of a specific VM, including a wait queue
/// and a list of tasks associated with the VCpus.
pub struct VMVCpus {
    // The ID of the VM to which these VCpus belong.
    _vm_id: usize,
    // A wait queue to manage task scheduling for the VCpus.
    wait_queue: crate::WaitQueue,
    // A map of tasks associated with the VCpus of this VM, keyed by vCPU ID.
    vcpu_task_list: Mutex<BTreeMap<usize, crate::AxTaskRef>>,
    // Pending virtual interrupts that must be injected by the owning vCPU task.
    pending_interrupts: Mutex<BTreeMap<usize, Vec<usize>>>,
    /// The number of currently running or halting VCpus. Used to track when the VM is fully
    /// shutdown.
    ///
    /// This number is incremented when a VCpu starts running and decremented when it exits because
    /// of the VM being shutdown.
    running_halting_vcpu_count: AtomicUsize,
}

impl VMVCpus {
    /// Creates a new `VMVCpus` instance for the given VM.
    ///
    /// # Arguments
    ///
    /// * `vm` - A reference to the VM for which the VCpus are being created.
    ///
    /// # Returns
    ///
    /// A new `VMVCpus` instance with an empty task list and a fresh wait queue.
    fn new(vm: VMRef) -> Self {
        Self {
            _vm_id: vm.id(),
            wait_queue: crate::WaitQueue::new(),
            vcpu_task_list: Mutex::new(BTreeMap::new()),
            pending_interrupts: Mutex::new(BTreeMap::new()),
            running_halting_vcpu_count: AtomicUsize::new(0),
        }
    }

    /// Adds a VCpu task to the list of VCpu tasks for this VM.
    ///
    /// # Arguments
    ///
    /// * `vcpu_task` - A reference to the task associated with a VCpu that is to be added.
    fn add_vcpu_task(&self, vcpu_id: usize, vcpu_task: crate::AxTaskRef) {
        self.vcpu_task_list.lock().insert(vcpu_id, vcpu_task);
        self.pending_interrupts.lock().entry(vcpu_id).or_default();
    }

    fn queue_interrupt(&self, vcpu_id: usize, vector: usize) -> AxResult<usize> {
        let task = self
            .vcpu_task_list
            .lock()
            .get(&vcpu_id)
            .cloned()
            .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} task not found")))?;
        let mut pending = self.pending_interrupts.lock();
        pending.entry(vcpu_id).or_default().push(vector);
        Ok(task.cpu_id() as usize)
    }

    fn drain_pending_interrupts(&self, vcpu_id: usize) -> Vec<usize> {
        let mut pending = self.pending_interrupts.lock();
        pending
            .get_mut(&vcpu_id)
            .map(core::mem::take)
            .unwrap_or_default()
    }

    /// Blocks the current thread on the wait queue associated with the VCpus of this VM.
    fn wait(&self) {
        self.wait_queue.wait()
    }

    /// Blocks the current thread on the wait queue associated with the VCpus of this VM
    /// until the provided condition is met.
    fn wait_until<F>(&self, condition: F)
    where
        F: Fn() -> bool,
    {
        self.wait_queue.wait_until(condition)
    }

    #[allow(dead_code)]
    fn notify_one(&self) {
        // FIXME: `WaitQueue::len` is removed
        // info!("Current wait queue length: {}", self.wait_queue.len());
        self.wait_queue.notify_one(false);
    }

    /// Notify all waiting vCPU threads to wake up.
    /// This is useful when shutting down a VM to ensure all vCPUs can check the shutdown flag.
    fn notify_all(&self) {
        self.wait_queue.notify_all(false);
    }

    /// Increments the count of running or halting VCpus by one.
    fn mark_vcpu_running(&self) {
        self.running_halting_vcpu_count
            .fetch_add(1, Ordering::Relaxed);
        // Relaxed is enough here, as we only need to ensure that the count is incremented and
        // decremented correctly, and there is no other data synchronization needed.
    }

    /// Decrements the count of running or halting VCpus by one. Returns true if this was the last
    /// VCpu to exit.
    fn mark_vcpu_exiting(&self) -> bool {
        self.running_halting_vcpu_count.fetch_update(
            Ordering::Relaxed,
            Ordering::Relaxed,
            |count| count.checked_sub(1),
        ) == Ok(1)
        // Relaxed is enough here, as we only need to ensure that the count is incremented and
        // decremented correctly, and there is no other data synchronization needed.
    }
}

/// Blocks the current thread until it is explicitly woken up, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu wait queue is used to block the current thread.
fn wait(vm_id: usize) {
    if let Some(vm_vcpus) = get_vm_vcpus(vm_id) {
        vm_vcpus.wait();
    } else {
        warn!("VM[{vm_id}] vCPU wait queue not found");
    }
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu wait queue is used to block the current thread.
/// * `condition` - A closure that returns a boolean value indicating whether the condition is met.
fn wait_for<F>(vm_id: usize, condition: F)
where
    F: Fn() -> bool,
{
    if let Some(vm_vcpus) = get_vm_vcpus(vm_id) {
        vm_vcpus.wait_until(condition);
    } else {
        warn!("VM[{vm_id}] vCPU wait queue not found");
    }
}

/// Notifies the primary VCpu task associated with the specified VM to wake up and resume execution.
/// This function is used to notify the primary VCpu of a VM to start running after the VM has been booted.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus are to be notified.
pub(crate) fn notify_primary_vcpu(vm_id: usize) {
    // Generally, the primary VCpu is the first and **only** VCpu in the list.
    if let Some(vm_vcpus) = get_vm_vcpus(vm_id) {
        vm_vcpus.notify_one();
    } else {
        warn!("VM[{vm_id}] vCPU resources not found");
    }
}

/// Notifies all VCpu tasks associated with the specified VM to wake up.
/// This is useful when shutting down a VM to ensure all waiting vCPUs can check the shutdown flag.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus should be notified.
pub(crate) fn notify_all_vcpus(vm_id: usize) {
    if let Some(vm_vcpus) = get_vm_vcpus(vm_id) {
        vm_vcpus.notify_all();
    }
}

pub(crate) fn queue_interrupt(vm_id: usize, vcpu_id: usize, vector: usize) -> AxResult {
    let vm_vcpus = get_vm_vcpus(vm_id)
        .ok_or_else(|| ax_err_type!(NotFound, format!("VM[{vm_id}] vCPU resources not found")))?;
    let cpu_id = vm_vcpus.queue_interrupt(vcpu_id, vector)?;
    vm_vcpus.notify_all();
    crate::send_host_ipi(cpu_id);
    Ok(())
}

fn drain_pending_interrupts(vm_id: usize, vcpu_id: usize, vcpu: &VCpuRef) -> AxResult {
    let Some(vm_vcpus) = get_vm_vcpus(vm_id) else {
        return Ok(());
    };

    for vector in vm_vcpus.drain_pending_interrupts(vcpu_id) {
        trace!("Injecting queued interrupt {vector:#x} into VM[{vm_id}] VCpu[{vcpu_id}]");
        vcpu.inject_interrupt(vector)?;
    }
    Ok(())
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
    if let Some(vm_vcpus) = VM_VCPU_TASKS.lock().remove(&vm_id) {
        // Take task references out before joining so we never block while
        // holding the per-VM task-list lock.
        let tasks: Vec<_> = vm_vcpus.vcpu_task_list.lock().values().cloned().collect();
        let task_count = tasks.len();

        info!("VM[{}] Joining {} VCpu tasks...", vm_id, task_count);

        // Join all VCpu tasks to ensure they have fully exited and cleaned up
        for (idx, task) in tasks.iter().enumerate() {
            debug!(
                "VM[{}] Joining VCpu task[{}]: {}",
                vm_id,
                idx,
                task.id_name()
            );
            let exit_code = task.join();
            debug!(
                "VM[{}] VCpu task[{}] exited with code: {}",
                vm_id, idx, exit_code
            );
        }

        info!(
            "VM[{}] VCpu resources cleaned up, {} VCpu tasks joined successfully",
            vm_id, task_count
        );
    } else {
        warn!("VM[{}] VCpu resources not found in queue", vm_id);
    }
}

/// Marks the VCpu of the specified VM as running.
fn mark_vcpu_running(vm_id: usize) {
    if let Some(vm_vcpus) = get_vm_vcpus(vm_id) {
        vm_vcpus.mark_vcpu_running();
    }
}

/// Marks the VCpu of the specified VM as exiting for VM shutdown. Returns true if this was the last
/// VCpu to exit.
fn mark_vcpu_exiting(vm_id: usize) -> bool {
    get_vm_vcpus(vm_id).is_some_and(|vm_vcpus| vm_vcpus.mark_vcpu_exiting())
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
fn vcpu_on(vm: VMRef, vcpu_id: usize, entry_point: GuestPhysAddr, arg: usize) -> AxResult {
    let vcpu = vm
        .vcpu_list()
        .get(vcpu_id)
        .cloned()
        .ok_or_else(|| ax_err_type!(NotFound, format!("vCPU {vcpu_id} not found")))?;
    if vcpu.state() != VCpuState::Free {
        return Err(ax_err_type!(
            BadState,
            format!("vCPU {} invalid state {:?}", vcpu.id(), vcpu.state())
        ));
    }

    vcpu.set_entry(entry_point)?;
    #[cfg(not(target_arch = "riscv64"))]
    vcpu.set_gpr(0, arg);

    #[cfg(target_arch = "riscv64")]
    {
        info!(
            "vcpu_on: vcpu[{}] entry={:x} opaque={:x}",
            vcpu_id, entry_point, arg
        );
        vcpu.set_gpr(crate::RiscvGprIndex::A0 as usize, vcpu_id);
        vcpu.set_gpr(crate::RiscvGprIndex::A1 as usize, arg);
    }

    let vcpu_task = alloc_vcpu_task(&vm, vcpu);

    let vm_vcpus = get_vm_vcpus(vm.id()).ok_or_else(|| {
        ax_err_type!(
            NotFound,
            format!("VM[{}] vCPU resources not found", vm.id())
        )
    })?;
    vm_vcpus.add_vcpu_task(vcpu_id, vcpu_task);
    Ok(())
}

/// Sets up the primary VCpu for the given VM,
/// generally the first VCpu in the VCpu list,
/// and initializing their respective wait queues and task lists.
/// VM's secondary VCpus are not started at this point.
///
/// # Arguments
///
/// * `vm` - A reference to the VM for which the VCpus are being set up.
pub fn setup_vm_primary_vcpu(vm: VMRef) {
    info!("Initializing VM[{}]'s {} vcpus", vm.id(), vm.vcpu_num());
    let vm_id = vm.id();
    if get_vm_vcpus(vm_id).is_some() {
        debug!("VM[{vm_id}] vCPU resources already exist");
        return;
    }
    let vm_vcpus = Arc::new(VMVCpus::new(vm.clone()));

    let primary_vcpu_id = 0;

    let Some(primary_vcpu) = vm.vcpu_list().get(primary_vcpu_id).cloned() else {
        warn!("VM[{vm_id}] has no primary vCPU");
        return;
    };
    let primary_vcpu_task = alloc_vcpu_task(&vm, primary_vcpu);
    vm_vcpus.add_vcpu_task(0, primary_vcpu_task);

    VM_VCPU_TASKS.lock().insert(vm_id, vm_vcpus);
}

/// Allocates arceos task for vcpu, set the task's entry function to [`vcpu_run()`],
/// also initializes the CPU mask if the VCpu has a dedicated physical CPU set.
///
/// # Arguments
///
/// * `vm` - A reference to the VM for which the VCpu task is being allocated.
/// * `vcpu` - A reference to the VCpu for which the task is being allocated.
///
/// # Returns
///
/// A reference to the task that has been allocated for the VCpu.
///
/// # Note
///
/// * The task associated with the VCpu is created with a kernel stack size of 256 KiB.
/// * The task is created in blocked state and added to the wait queue directly,
///   instead of being added to the ready queue. It will be woken up by notify_primary_vcpu().
fn alloc_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> crate::AxTaskRef {
    info!("Spawning task for VM[{}] VCpu[{}]", vm.id(), vcpu.id());
    let mut vcpu_task = crate::TaskInner::new(
        vcpu_run,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        KERNEL_STACK_SIZE,
    );

    if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
        vcpu_task.set_cpumask(crate::host_cpu_mask_from_raw_bits(phys_cpu_set));
    }

    // Use Weak reference in TaskExt to avoid keeping VM alive
    let inner = VCpuTask::new(vm, vcpu);
    *vcpu_task.task_ext_mut() = Some(crate::AxTaskExt::from_impl(inner));

    info!(
        "VCpu task {} created {:?}",
        vcpu_task.id_name(),
        vcpu_task.cpumask()
    );
    crate::spawn_host_task(vcpu_task)
}

/// The main routine for VCpu task.
/// This function is the entry point for the VCpu tasks, which are spawned for each VCpu of a VM.
///
/// When the VCpu first starts running, it waits for the VM to be in the running state.
/// It then enters a loop where it runs the VCpu and handles the various exit reasons.
fn vcpu_run() {
    let curr = crate::current_host_task();

    let vm = curr.as_vcpu_task().vm();
    let vcpu = curr.as_vcpu_task().vcpu.clone();
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();

    info!("VM[{}] VCpu[{}] waiting for running", vm.id(), vcpu.id());
    wait_for(vm_id, || vm.running());

    info!("VM[{}] VCpu[{}] running...", vm.id(), vcpu.id());
    #[cfg(target_arch = "x86_64")]
    super::x86_irq::enable_ioapic_irq_forwarding(&vm, &vcpu);
    mark_vcpu_running(vm_id);

    loop {
        if let Err(err) = drain_pending_interrupts(vm_id, vcpu_id, &vcpu) {
            warn!("Failed to inject queued interrupts into VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}");
        }

        #[cfg(target_arch = "x86_64")]
        super::x86_irq::drain_pending_ioapic_irqs(&vm, &vcpu);

        match vm.run_vcpu(vcpu_id) {
            Ok(exit_reason) => match exit_reason {
                AxVCpuExitReason::Hypercall { nr, args } => {
                    debug!("Hypercall [{nr}] args {args:x?}");
                    use crate::runtime::hvc::HyperCall;

                    match HyperCall::new(vm.clone(), nr, args) {
                        Ok(hypercall) => {
                            let ret_val = match hypercall.execute() {
                                Ok(ret_val) => ret_val as isize,
                                Err(err) => {
                                    warn!("Hypercall [{nr:#x}] failed: {err:?}");
                                    -1
                                }
                            };
                            vcpu.set_return_value(ret_val as usize);
                        }
                        Err(err) => {
                            warn!("Hypercall [{nr:#x}] failed: {err:?}");
                        }
                    }
                }
                AxVCpuExitReason::FailEntry {
                    hardware_entry_failure_reason,
                } => {
                    warn!(
                        "VM[{vm_id}] VCpu[{vcpu_id}] run failed with exit code \
                         {hardware_entry_failure_reason}"
                    );
                }
                AxVCpuExitReason::ExternalInterrupt { vector } => {
                    debug!("VM[{vm_id}] run VCpu[{vcpu_id}] get irq {vector}");

                    // TODO: maybe move this irq dispatcher to lower layer to accelerate the interrupt handling
                    #[cfg(not(any(target_arch = "aarch64", target_arch = "riscv64")))]
                    crate::dispatch_host_irq(vector as usize);
                    #[cfg(target_arch = "riscv64")]
                    vcpu.with_current_cpu_set(|| {
                        crate::dispatch_host_irq(vector as usize);
                        vcpu.get_arch_vcpu().latch_hvip_from_hw();
                    });
                    crate::check_timer_events();
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::forward_passthrough_irq_from_vmexit(
                        &vm,
                        &vcpu,
                        vector as usize,
                    );
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_pending_serial_irq(&vm, &vcpu);
                }
                AxVCpuExitReason::PreemptionTimer => {
                    crate::timer::check_events();
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_due_pit_irq0(&vm, &vcpu);
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_pending_serial_irq(&vm, &vcpu);
                }
                AxVCpuExitReason::InterruptEnd { vector: _vector } => {
                    #[cfg(target_arch = "x86_64")]
                    if let Some(vector) = _vector {
                        super::x86_irq::inject_pending_ioapic_irq_after_eoi(&vm, &vcpu, vector);
                    }
                }
                AxVCpuExitReason::Halt => {
                    debug!("VM[{vm_id}] run VCpu[{vcpu_id}] Halt");
                    #[cfg(target_arch = "x86_64")]
                    super::x86_irq::inject_pending_serial_irq(&vm, &vcpu);
                    #[cfg(target_arch = "x86_64")]
                    continue;
                    #[cfg(not(target_arch = "x86_64"))]
                    wait(vm_id)
                }
                AxVCpuExitReason::Nothing => {}
                AxVCpuExitReason::CpuDown { _state } => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] CpuDown state {_state:#x}");
                    wait(vm_id)
                }
                AxVCpuExitReason::CpuUp {
                    target_cpu,
                    entry_point,
                    arg,
                } => {
                    info!(
                        "VM[{vm_id}]'s VCpu[{vcpu_id}] try to boot target_cpu [{target_cpu}] \
                         entry_point={entry_point:x} arg={arg:#x}"
                    );

                    // Get the mapping relationship between all vCPUs and physical CPUs from the configuration
                    let vcpu_mappings = vm.get_vcpu_affinities_pcpu_ids();

                    // Find the vCPU ID corresponding to the physical ID
                    let Some(target_vcpu_id) =
                        vcpu_mappings.iter().find_map(|(vcpu_id, _, phys_id)| {
                            (*phys_id == target_cpu as usize).then_some(*vcpu_id)
                        })
                    else {
                        warn!("Physical CPU ID {target_cpu} not found in VM configuration");
                        vcpu.set_return_value(usize::MAX);
                        continue;
                    };

                    match vcpu_on(vm.clone(), target_vcpu_id, entry_point, arg as _) {
                        Ok(()) => {
                            #[cfg(not(target_arch = "riscv64"))]
                            vcpu.set_gpr(0, 0);
                            #[cfg(target_arch = "riscv64")]
                            vcpu.set_gpr(crate::RiscvGprIndex::A0 as usize, 0);
                        }
                        Err(err) => {
                            warn!("Failed to boot VM[{vm_id}] VCpu[{target_vcpu_id}]: {err:?}");
                            vcpu.set_return_value(usize::MAX);
                        }
                    }
                }
                AxVCpuExitReason::SystemDown => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] SystemDown");
                    if let Err(err) = vm.shutdown() {
                        warn!("VM[{vm_id}] shutdown failed: {err:?}");
                    }
                    // Notify all vCPUs to wake up to check the shutdown flag
                    notify_all_vcpus(vm_id);
                }
                AxVCpuExitReason::SendIPI {
                    target_cpu,
                    target_cpu_aux,
                    send_to_all,
                    send_to_self,
                    vector,
                } => {
                    debug!(
                        "VM[{vm_id}] run VCpu[{vcpu_id}] SendIPI, target_cpu={target_cpu:#x}, \
                         target_cpu_aux={target_cpu_aux:#x}, vector={vector}",
                    );
                    if send_to_all {
                        warn!("Send IPI to all CPUs is not implemented yet");
                        continue;
                    }

                    if target_cpu == vcpu_id as u64 || send_to_self {
                        crate::inject_current_vcpu_interrupt(vector as _)
                            .expect("failed to inject self IPI into current vCPU");
                    } else if let Err(err) =
                        vm.inject_interrupt_to_vcpu(CpuMask::one_shot(target_cpu as _), vector as _)
                    {
                        warn!(
                            "Failed to inject interrupt {vector} to VM[{vm_id}] CPU {target_cpu}: \
                             {err:?}"
                        );
                    }
                }
                e => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] unhandled vmexit: {e:?}");
                }
            },
            Err(err) => {
                error!("VM[{vm_id}] run VCpu[{vcpu_id}] get error {err:?}");
                if let Err(err) = vm.shutdown() {
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
            wait_for(vm_id, || !vm.suspending());
            info!("VM[{}] VCpu[{}] resumed from suspend", vm_id, vcpu_id);
            continue;
        }

        // Check if the VM is stopping.
        if vm.stopping() {
            warn!(
                "VM[{}] VCpu[{}] stopping because of VM stopping",
                vm_id, vcpu_id
            );

            if mark_vcpu_exiting(vm_id) {
                info!("VM[{vm_id}] VCpu[{vcpu_id}] last VCpu exiting, decreasing running VM count");

                // Transition from Stopping to Stopped
                vm.set_vm_status(crate::VMStatus::Stopped);
                info!("VM[{}] state changed to Stopped", vm_id);

                #[cfg(target_arch = "x86_64")]
                super::x86_irq::disable_ioapic_irq_forwarding_for_vm(vm_id);

                sub_running_vm_count(1);
                crate::host_wait_queue_wake(&super::VMM, 1);
            }

            break;
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}
