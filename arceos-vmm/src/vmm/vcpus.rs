use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use std::os::arceos::api;
use std::os::arceos::modules::axtask;

use axaddrspace::GuestPhysAddr;
use axtask::{AxTaskRef, TaskExtRef, TaskInner, WaitQueue};
use axvcpu::{AxVCpuExitReason, VCpuState};
use axvm::AxVCpuRef;

use api::sys::ax_terminate;
use api::task::AxCpuMask;

use crate::task::TaskExt;
use crate::vmm::VMRef;

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

/// A global static BTreeMap that holds the wait queues for vCPUs
/// associated with their respective VMs, identified by their VM IDs.
///
/// TODO: find a better data structure to replace the `static mut`.
static mut VM_VCPU_TASK_WAIT_QUEUE: BTreeMap<usize, VMVcpus> = BTreeMap::new();

/// A structure representing the vCPUs of a specific VM, including a wait queue
/// and a list of tasks associated with the vCPUs.
pub struct VMVcpus {
    // The ID of the VM to which these vCPUs belong.
    _vm_id: usize,
    // A wait queue to manage task scheduling for the vCPUs.
    wait_queue: WaitQueue,
    // A list of tasks associated with the vCPUs of this VM.
    vcpu_task_list: Vec<AxTaskRef>,
}

impl VMVcpus {
    /// Creates a new `VMVcpus` instance for the given VM.
    ///
    /// # Arguments
    ///
    /// * `vm` - A reference to the VM for which the vCPUs are being created.
    ///
    /// # Returns
    ///
    /// A new `VMVcpus` instance with an empty task list and a fresh wait queue.
    fn new(vm: VMRef) -> Self {
        Self {
            _vm_id: vm.id(),
            wait_queue: WaitQueue::new(),
            vcpu_task_list: Vec::with_capacity(vm.vcpu_num()),
        }
    }

    /// Adds a vCPU task to the list of vCPU tasks for this VM.
    ///
    /// # Arguments
    ///
    /// * `vcpu_task` - A reference to the task associated with a vCPU that is to be added.
    fn add_vcpu_task(&mut self, vcpu_task: AxTaskRef) {
        self.vcpu_task_list.push(vcpu_task);
    }

    /// Blocks the current thread on the wait queue associated with the vCPUs of this VM.
    fn wait(&self) {
        self.wait_queue.wait()
    }

    /// Blocks the current thread on the wait queue associated with the vCPUs of this VM
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
}

/// Blocks the current thread until it is explicitly woken up, using the wait queue
/// associated with the vCPUs of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose vCPU wait queue is used to block the current thread.
///
fn wait(vm_id: usize) {
    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get(&vm_id) }
        .unwrap()
        .wait()
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the vCPUs of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose vCPU wait queue is used to block the current thread.
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

/// Notifies the primary vCPU task associated with the specified VM to wake up and resume execution.
/// This function is used to notify the primary vCPU of a VM to start running after the VM has been booted.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose vCPUs are to be notified.
///
pub(crate) fn notify_primary_vcpu(vm_id: usize) {
    // Generally, the primary vCPU is the first and **only** vCPU in the list.
    unsafe { VM_VCPU_TASK_WAIT_QUEUE.get_mut(&vm_id) }
        .unwrap()
        .notify_one()
}

/// Boot target vCPU on the specified VM.
/// This function is used to boot a secondary vCPU on a VM, setting the entry point and argument for the vCPU.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM on which the vCPU is to be booted.
/// * `vcpu_id` - The ID of the vCPU to be booted.
/// * `entry_point` - The entry point of the vCPU.
/// * `arg` - The argument to be passed to the vCPU.
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

/// Sets up the primary vCPU for the given VM,
/// generally the first vCPU in the vCPU list,
/// and initializing their respective wait queues and task lists.
/// VM's secondary vCPUs are not started at this point.
///
/// # Arguments
///
/// * `vm` - A reference to the VM for which the vCPUs are being set up.
pub fn setup_vm_primary_vcpu(vm: VMRef) {
    info!("Initializing VM[{}]'s {} vcpus", vm.id(), vm.vcpu_num());
    let vm_id = vm.id();
    let mut vm_vcpus = VMVcpus::new(vm.clone());

    let primary_vcpu_id = 0;

    let primary_vcpu = vm.vcpu_list()[primary_vcpu_id].clone();
    let primary_vcpu_task = alloc_vcpu_task(vm.clone(), primary_vcpu);
    vm_vcpus.add_vcpu_task(primary_vcpu_task);
    unsafe {
        VM_VCPU_TASK_WAIT_QUEUE.insert(vm_id, vm_vcpus);
    }
}

/// Allocates arceos task for vcpu, set the task's entry function to [`vcpu_run()`],
/// alse initializes the CPU mask if the vCPU has a dedicated physical CPU set.
///
/// # Arguments
///
/// * `vm` - A reference to the VM for which the vCPU task is being allocated.
/// * `vcpu` - A reference to the vCPU for which the task is being allocated.
///
/// # Returns
///
/// A reference to the task that has been allocated for the vCPU.
///
/// # Note
///
/// * The task associated with the vCPU is created with a kernel stack size of 256 KiB.
/// * The task is scheduled on the scheduler of arceos after it is spawned.
fn alloc_vcpu_task(vm: VMRef, vcpu: AxVCpuRef) -> AxTaskRef {
    info!("Spawning task for VM[{}] Vcpu[{}]", vm.id(), vcpu.id());
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
        "Vcpu task {} created {:?}",
        vcpu_task.id_name(),
        vcpu_task.cpumask()
    );
    axtask::spawn_task(vcpu_task)
}

/// The main routine for vCPU task.
/// This function is the entry point for the vCPU tasks, which are spawned for each vCPU of a VM.
///
/// When the vCPU first starts running, it waits for the VM to be in the running state.
/// It then enters a loop where it runs the vCPU and handles the various exit reasons.
fn vcpu_run() {
    let curr = axtask::current();

    let vm = curr.task_ext().vm.clone();
    let vcpu = curr.task_ext().vcpu.clone();
    let vm_id = vm.id();
    let vcpu_id = vcpu.id();

    info!("VM[{}] Vcpu[{}] waiting for running", vm.id(), vcpu.id());
    wait_for(vm_id, || vm.running());

    info!("VM[{}] Vcpu[{}] running...", vm.id(), vcpu.id());

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
                    ax_terminate()
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
    }
}
