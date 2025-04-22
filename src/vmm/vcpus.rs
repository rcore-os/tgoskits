use alloc::{collections::BTreeMap, vec::Vec};

use core::sync::atomic::{AtomicUsize, Ordering};
use std::os::arceos::{
    api::task::{AxCpuMask, ax_wait_queue_wake},
    modules::axtask,
};

use axaddrspace::GuestPhysAddr;
use axtask::{AxTaskRef, TaskExtRef, TaskInner, WaitQueue};
use axvcpu::{AxVCpuExitReason, VCpuState};

use crate::task::TaskExt;
use crate::vmm::{VCpuRef, VMRef};

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

/// A global static BTreeMap that holds the wait queues for VCpus
/// associated with their respective VMs, identified by their VM IDs.
///
/// TODO: find a better data structure to replace the `static mut`, something like a conditional
/// variable.
static mut VM_VCPU_TASK_WAIT_QUEUE: BTreeMap<usize, VMVCpus> = BTreeMap::new();

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

    fn notify_one(&mut self) {
        self.wait_queue.notify_one(false);
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
    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get(&vm_id) }
        .unwrap()
        .wait()
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
    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get(&vm_id) }
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
    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get_mut(&vm_id) }
        .unwrap()
        .notify_one()
}

/// Marks the VCpu of the specified VM as running.
fn mark_vcpu_running(vm_id: usize) {
    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get(&vm_id) }
        .unwrap()
        .mark_vcpu_running();
}

/// Marks the VCpu of the specified VM as exiting for VM shutdown. Returns true if this was the last
/// VCpu to exit.
fn mark_vcpu_exiting(vm_id: usize) -> bool {
    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get(&vm_id) }
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
    vcpu.set_gpr(0, arg);

    #[cfg(target_arch = "riscv64")]
    {
        debug!(
            "vcpu_on: vcpu[{}] entry={:x} opaque={:x}",
            vcpu_id, entry_point, arg
        );
        vcpu.set_gpr(0, vcpu_id);
        vcpu.set_gpr(1, arg);
    }

    let vcpu_task = alloc_vcpu_task(vm.clone(), vcpu);

    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get_mut(&vm.id()) }
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
    let primary_vcpu_task = alloc_vcpu_task(vm.clone(), primary_vcpu);
    vm_vcpus.add_vcpu_task(primary_vcpu_task);
    unsafe {
        VM_VCPU_TASK_WAIT_QUEUE.insert(vm_id, vm_vcpus);
    }
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
/// * The task is scheduled on the scheduler of arceos after it is spawned.
fn alloc_vcpu_task(vm: VMRef, vcpu: VCpuRef) -> AxTaskRef {
    info!("Spawning task for VM[{}] VCpu[{}]", vm.id(), vcpu.id());
    let mut vcpu_task = TaskInner::new(
        vcpu_run,
        format!("VM[{}]-VCpu[{}]", vm.id(), vcpu.id()),
        KERNEL_STACK_SIZE,
    );

    if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
        vcpu_task.set_cpumask(AxCpuMask::from_raw_bits(phys_cpu_set));
    }
    vcpu_task.init_task_ext(TaskExt::new(vm, vcpu));

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

    let vm = curr.task_ext().vm.clone();
    let vcpu = curr.task_ext().vcpu.clone();
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();

    info!("VM[{}] VCpu[{}] waiting for running", vm.id(), vcpu.id());
    wait_for(vm_id, || vm.running());

    info!("VM[{}] VCpu[{}] running...", vm.id(), vcpu.id());
    mark_vcpu_running(vm_id);

    loop {
        match vm.run_vcpu(vcpu_id) {
            // match vcpu.run() {
            Ok(exit_reason) => match exit_reason {
                AxVCpuExitReason::Hypercall { nr, args } => {
                    debug!("Hypercall [{}] args {:x?}", nr, args);
                }
                AxVCpuExitReason::FailEntry {
                    hardware_entry_failure_reason,
                } => {
                    warn!(
                        "VM[{}] VCpu[{}] run failed with exit code {}",
                        vm_id, vcpu_id, hardware_entry_failure_reason
                    );
                }
                AxVCpuExitReason::ExternalInterrupt { vector } => {
                    debug!("VM[{}] run VCpu[{}] get irq {}", vm_id, vcpu_id, vector);
                }
                AxVCpuExitReason::Halt => {
                    debug!("VM[{}] run VCpu[{}] Halt", vm_id, vcpu_id);
                    wait(vm_id)
                }
                AxVCpuExitReason::Nothing => {}
                AxVCpuExitReason::CpuDown { _state } => {
                    warn!(
                        "VM[{}] run VCpu[{}] CpuDown state {:#x}",
                        vm_id, vcpu_id, _state
                    );
                    wait(vm_id)
                }
                AxVCpuExitReason::CpuUp {
                    target_cpu,
                    entry_point,
                    arg,
                } => {
                    info!(
                        "VM[{}]'s VCpu[{}] try to boot target_cpu [{}] entry_point={:x} arg={:#x}",
                        vm_id, vcpu_id, target_cpu, entry_point, arg
                    );
                    vcpu_on(vm.clone(), target_cpu as _, entry_point, arg as _);
                    vcpu.set_gpr(0, 0);
                }
                AxVCpuExitReason::SystemDown => {
                    warn!("VM[{}] run VCpu[{}] SystemDown", vm_id, vcpu_id);
                    vm.shutdown().expect("VM shutdown failed");
                }
                _ => {
                    warn!("Unhandled VM-Exit");
                }
            },
            Err(err) => {
                warn!("VM[{}] run VCpu[{}] get error {:?}", vm_id, vcpu_id, err);
                wait(vm_id)
            }
        }

        // Check if the VM is shutting down.
        if vm.shutting_down() {
            warn!(
                "VM[{}] VCpu[{}] shutting down because of VM shutdown",
                vm_id, vcpu_id
            );

            if mark_vcpu_exiting(vm_id) {
                info!(
                    "VM[{}] VCpu[{}] last VCpu exiting, decreasing running VM count",
                    vm_id, vcpu_id
                );

                super::RUNNING_VM_COUNT.fetch_sub(1, Ordering::Release);
                ax_wait_queue_wake(&super::VMM, 1);
            }

            break;
        }
    }

    info!("VM[{}] VCpu[{}] exiting...", vm_id, vcpu_id);
}
