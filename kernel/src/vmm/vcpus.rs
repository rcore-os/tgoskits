use alloc::{collections::BTreeMap, vec::Vec};
use cpumask::CpuMask;

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use std::os::arceos::{
    api::task::{AxCpuMask, ax_wait_queue_wake},
    modules::{
        axhal::{self, time::busy_wait},
        axtask::{self, AxTaskExt},
    },
};

use axaddrspace::GuestPhysAddr;
use axtask::{AxTaskRef, TaskInner, WaitQueue};
use axvcpu::{AxVCpuExitReason, VCpuState};

use crate::{hal::arch::inject_interrupt, task::VCpuTask};
use crate::{
    task::AsVCpuTask,
    vmm::{VCpuRef, VMRef, sub_running_vm_count},
};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

/// A global static BTreeMap that holds the wait queues for VCpus
/// associated with their respective VMs, identified by their VM IDs.
///
/// TODO: find a better data structure to replace the `static mut`, something like a conditional
/// variable.
static VM_VCPU_TASK_WAIT_QUEUE: Queue = Queue::new();

/// A thread-safe queue that manages wait queues for VCpus across multiple VMs.
///
/// This structure wraps a BTreeMap that maps VM IDs to their corresponding VMVCpus structures.
/// It provides thread-safe access to the mapping through interior mutability using UnsafeCell.
/// Each VM is identified by its unique ID, and the queue manages the VCpu tasks and wait
/// operations for all VMs in the system.
struct Queue(UnsafeCell<BTreeMap<usize, VMVCpus>>);

unsafe impl Sync for Queue {}
unsafe impl Send for Queue {}

impl Queue {
    /// Creates a new empty Queue.
    ///
    /// # Returns
    ///
    /// A new Queue instance with an empty BTreeMap.
    const fn new() -> Self {
        Self(UnsafeCell::new(BTreeMap::new()))
    }

    /// Retrieves a reference to the VMVCpus for the specified VM ID.
    fn get(&self, vm_id: &usize) -> Option<&VMVCpus> {
        unsafe { (*self.0.get()).get(vm_id) }
    }

    /// Retrieves a mutable reference to the VMVCpus for the specified VM ID.
    #[allow(clippy::mut_from_ref)]
    fn get_mut(&self, vm_id: &usize) -> Option<&mut VMVCpus> {
        unsafe { (*self.0.get()).get_mut(vm_id) }
    }

    /// Inserts a new VMVCpus entry for the specified VM ID.
    fn insert(&self, vm_id: usize, vcpus: VMVCpus) {
        unsafe {
            (*self.0.get()).insert(vm_id, vcpus);
        }
    }

    /// Removes the VMVCpus entry for the specified VM ID.
    fn remove(&self, vm_id: &usize) -> Option<VMVCpus> {
        unsafe { (*self.0.get()).remove(vm_id) }
    }
}

/// A structure representing the VCpus of a specific VM, including a wait queue
/// and a list of tasks associated with the VCpus.
pub struct VMVCpus {
    // The ID of the VM to which these VCpus belong.
    _vm_id: usize,
    // A wait queue to manage task scheduling for the VCpus.
    wait_queue: WaitQueue,
    // A list of tasks associated with the VCpus of this VM.
    vcpu_task_list: Vec<AxTaskRef>,
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
            wait_queue: WaitQueue::new(),
            vcpu_task_list: Vec::with_capacity(vm.vcpu_num()),
            running_halting_vcpu_count: AtomicUsize::new(0),
        }
    }

    /// Adds a VCpu task to the list of VCpu tasks for this VM.
    ///
    /// # Arguments
    ///
    /// * `vcpu_task` - A reference to the task associated with a VCpu that is to be added.
    fn add_vcpu_task(&mut self, vcpu_task: AxTaskRef) {
        // It may be dangerous to go lock-free here, as two VCpus may invoke `CpuUp` at the same
        // time. However, in most scenarios, only the bsp will `CpuUp` other VCpus, making this
        // operation single-threaded. Therefore, we just tolerate this as for now.
        self.vcpu_task_list.push(vcpu_task);
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
    fn notify_one(&mut self) {
        // FIXME: `WaitQueue::len` is removed
        // info!("Current wait queue length: {}", self.wait_queue.len());
        self.wait_queue.notify_one(false);
    }

    /// Notify all waiting vCPU threads to wake up.
    /// This is useful when shutting down a VM to ensure all vCPUs can check the shutdown flag.
    fn notify_all(&mut self) {
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
        self.running_halting_vcpu_count
            .fetch_sub(1, Ordering::Relaxed)
            == 1
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
///
fn wait(vm_id: usize) {
    VM_VCPU_TASK_WAIT_QUEUE.get(&vm_id).unwrap().wait()
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the VCpus of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpu wait queue is used to block the current thread.
/// * `condition` - A closure that returns a boolean value indicating whether the condition is met.
///
fn wait_for<F>(vm_id: usize, condition: F)
where
    F: Fn() -> bool,
{
    VM_VCPU_TASK_WAIT_QUEUE
        .get(&vm_id)
        .unwrap()
        .wait_until(condition)
}

/// Notifies the primary VCpu task associated with the specified VM to wake up and resume execution.
/// This function is used to notify the primary VCpu of a VM to start running after the VM has been booted.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus are to be notified.
///
pub(crate) fn notify_primary_vcpu(vm_id: usize) {
    // Generally, the primary VCpu is the first and **only** VCpu in the list.
    VM_VCPU_TASK_WAIT_QUEUE
        .get_mut(&vm_id)
        .unwrap()
        .notify_one()
}

/// Notifies all VCpu tasks associated with the specified VM to wake up.
/// This is useful when shutting down a VM to ensure all waiting vCPUs can check the shutdown flag.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose VCpus should be notified.
///
pub(crate) fn notify_all_vcpus(vm_id: usize) {
    if let Some(vm_vcpus) = VM_VCPU_TASK_WAIT_QUEUE.get_mut(&vm_id) {
        vm_vcpus.notify_all();
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
    if let Some(vm_vcpus) = VM_VCPU_TASK_WAIT_QUEUE.remove(&vm_id) {
        let task_count = vm_vcpus.vcpu_task_list.len();

        info!("VM[{}] Joining {} VCpu tasks...", vm_id, task_count);

        // Join all VCpu tasks to ensure they have fully exited and cleaned up
        for (idx, task) in vm_vcpus.vcpu_task_list.iter().enumerate() {
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
    VM_VCPU_TASK_WAIT_QUEUE
        .get(&vm_id)
        .unwrap()
        .mark_vcpu_running();
}

/// Marks the VCpu of the specified VM as exiting for VM shutdown. Returns true if this was the last
/// VCpu to exit.
fn mark_vcpu_exiting(vm_id: usize) -> bool {
    VM_VCPU_TASK_WAIT_QUEUE
        .get(&vm_id)
        .unwrap()
        .mark_vcpu_exiting()
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
///
fn vcpu_on(vm: VMRef, vcpu_id: usize, entry_point: GuestPhysAddr, arg: usize) {
    let vcpu = vm.vcpu_list()[vcpu_id].clone();
    assert_eq!(
        vcpu.state(),
        VCpuState::Free,
        "vcpu_on: {} invalid vcpu state {:?}",
        vcpu.id(),
        vcpu.state()
    );

    vcpu.set_entry(entry_point)
        .expect("vcpu_on: set_entry failed");
    #[cfg(not(target_arch = "riscv64"))]
    vcpu.set_gpr(0, arg);

    #[cfg(target_arch = "riscv64")]
    {
        info!(
            "vcpu_on: vcpu[{}] entry={:x} opaque={:x}",
            vcpu_id, entry_point, arg
        );
        vcpu.set_gpr(riscv_vcpu::GprIndex::A0 as usize, vcpu_id);
        vcpu.set_gpr(riscv_vcpu::GprIndex::A1 as usize, arg);
    }

    let vcpu_task = alloc_vcpu_task(&vm, vcpu);

    VM_VCPU_TASK_WAIT_QUEUE
        .get_mut(&vm.id())
        .unwrap()
        .add_vcpu_task(vcpu_task);
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
    let mut vm_vcpus = VMVCpus::new(vm.clone());

    let primary_vcpu_id = 0;

    let primary_vcpu = vm.vcpu_list()[primary_vcpu_id].clone();
    let primary_vcpu_task = alloc_vcpu_task(&vm, primary_vcpu);
    vm_vcpus.add_vcpu_task(primary_vcpu_task);

    VM_VCPU_TASK_WAIT_QUEUE.insert(vm_id, vm_vcpus);
}

/// Finds the [`AxTaskRef`] associated with the specified vCPU of the specified VM.
// pub fn find_vcpu_task(vm_id: usize, vcpu_id: usize) -> Option<AxTaskRef> {
//     with_vcpu_task(vm_id, vcpu_id, |task| task.clone())
// }
/// Executes the provided closure with the [`AxTaskRef`] associated with the specified vCPU of the specified VM.
pub fn with_vcpu_task<T, F: FnOnce(&AxTaskRef) -> T>(
    vm_id: usize,
    vcpu_id: usize,
    f: F,
) -> Option<T> {
    VM_VCPU_TASK_WAIT_QUEUE
        .get(&vm_id)
        .unwrap()
        .vcpu_task_list
        .get(vcpu_id)
        .map(f)
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
fn alloc_vcpu_task(vm: &VMRef, vcpu: VCpuRef) -> AxTaskRef {
    info!("Spawning task for VM[{}] VCpu[{}]", vm.id(), vcpu.id());
    let mut vcpu_task = TaskInner::new(
        vcpu_run,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        KERNEL_STACK_SIZE,
    );

    if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
        vcpu_task.set_cpumask(AxCpuMask::from_raw_bits(phys_cpu_set));
    }

    // Use Weak reference in TaskExt to avoid keeping VM alive
    let inner = VCpuTask::new(vm, vcpu);
    *vcpu_task.task_ext_mut() = Some(unsafe { AxTaskExt::from_impl(inner) });

    info!(
        "VCpu task {} created {:?}",
        vcpu_task.id_name(),
        vcpu_task.cpumask()
    );
    axtask::spawn_task(vcpu_task)
}

/// The main routine for VCpu task.
/// This function is the entry point for the VCpu tasks, which are spawned for each VCpu of a VM.
///
/// When the VCpu first starts running, it waits for the VM to be in the running state.
/// It then enters a loop where it runs the VCpu and handles the various exit reasons.
fn vcpu_run() {
    let curr = axtask::current();

    let vm = curr.as_vcpu_task().vm();
    let vcpu = curr.as_vcpu_task().vcpu.clone();
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();

    // boot delay
    let boot_delay_sec = (vm_id - 1) * 5;
    info!("VM[{vm_id}] boot delay: {boot_delay_sec}s");
    busy_wait(Duration::from_secs(boot_delay_sec as _));

    info!("VM[{}] VCpu[{}] waiting for running", vm.id(), vcpu.id());
    wait_for(vm_id, || vm.running());

    info!("VM[{}] VCpu[{}] running...", vm.id(), vcpu.id());
    mark_vcpu_running(vm_id);

    loop {
        match vm.run_vcpu(vcpu_id) {
            Ok(exit_reason) => match exit_reason {
                AxVCpuExitReason::Hypercall { nr, args } => {
                    debug!("Hypercall [{nr}] args {args:x?}");
                    use crate::vmm::hvc::HyperCall;

                    match HyperCall::new(vcpu.clone(), vm.clone(), nr, args) {
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
                        "VM[{vm_id}] VCpu[{vcpu_id}] run failed with exit code {hardware_entry_failure_reason}"
                    );
                }
                AxVCpuExitReason::ExternalInterrupt { vector } => {
                    debug!("VM[{vm_id}] run VCpu[{vcpu_id}] get irq {vector}");

                    // TODO: maybe move this irq dispatcher to lower layer to accelerate the interrupt handling
                    axhal::irq::irq_handler(vector as usize);
                    super::timer::check_events();
                }
                AxVCpuExitReason::Halt => {
                    debug!("VM[{vm_id}] run VCpu[{vcpu_id}] Halt");
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
                        "VM[{vm_id}]'s VCpu[{vcpu_id}] try to boot target_cpu [{target_cpu}] entry_point={entry_point:x} arg={arg:#x}"
                    );

                    // Get the mapping relationship between all vCPUs and physical CPUs from the configuration
                    let vcpu_mappings = vm.get_vcpu_affinities_pcpu_ids();

                    // Find the vCPU ID corresponding to the physical ID
                    let target_vcpu_id = vcpu_mappings
                        .iter()
                        .find_map(|(vcpu_id, _, phys_id)| {
                            if *phys_id == target_cpu as usize {
                                Some(*vcpu_id)
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| {
                            panic!("Physical CPU ID {target_cpu} not found in VM configuration",)
                        });

                    vcpu_on(vm.clone(), target_vcpu_id, entry_point, arg as _);
                    #[cfg(not(target_arch = "riscv64"))]
                    vcpu.set_gpr(0, 0);
                    #[cfg(target_arch = "riscv64")]
                    vcpu.set_gpr(riscv_vcpu::GprIndex::A0 as usize, 0);
                }
                AxVCpuExitReason::SystemDown => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] SystemDown");
                    vm.shutdown().expect("VM shutdown failed");
                }
                AxVCpuExitReason::SendIPI {
                    target_cpu,
                    target_cpu_aux,
                    send_to_all,
                    send_to_self,
                    vector,
                } => {
                    debug!(
                        "VM[{vm_id}] run VCpu[{vcpu_id}] SendIPI, target_cpu={target_cpu:#x}, target_cpu_aux={target_cpu_aux:#x}, vector={vector}",
                    );
                    if send_to_all {
                        unimplemented!("Send IPI to all CPUs is not implemented yet");
                    }

                    if target_cpu == vcpu_id as u64 || send_to_self {
                        inject_interrupt(vector as _);
                    } else {
                        vm.inject_interrupt_to_vcpu(
                            CpuMask::one_shot(target_cpu as _),
                            vector as _,
                        )
                        .unwrap();
                    }
                }
                e => {
                    warn!("VM[{vm_id}] run VCpu[{vcpu_id}] unhandled vmexit: {e:?}");
                }
            },
            Err(err) => {
                error!("VM[{vm_id}] run VCpu[{vcpu_id}] get error {err:?}");
                // wait(vm_id)
                vm.shutdown().expect("VM shutdown failed");
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
                vm.set_vm_status(axvm::VMStatus::Stopped);
                info!("VM[{}] state changed to Stopped", vm_id);

                sub_running_vm_count(1);
                ax_wait_queue_wake(&super::VMM, 1);
            }

            break;
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}
