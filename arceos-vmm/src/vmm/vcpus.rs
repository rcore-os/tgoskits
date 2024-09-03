use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use spin::Mutex;

use axtask::{AxTaskRef, TaskExtRef, TaskInner, WaitQueue};
use axvcpu::AxVCpuExitReason;

use crate::task::TaskExt;
use crate::vmm::VMRef;

const KERNEL_STACK_SIZE: usize = 0x40000; // 256 KiB

/// A global static mutex-protected BTreeMap that holds the wait queues for vCPUs
/// associated with their respective VMs, identified by their VM IDs.
static VM_VCPU_TASK_WAIT_QUEUE: Mutex<BTreeMap<usize, VMVcpus>> = Mutex::new(BTreeMap::new());

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
        self.vcpu_task_list.push(vcpu_task)
    }
}

/// Blocks the current thread until it is explicitly woken up, using the wait queue
/// associated with the vCPUs of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose vCPU wait queue is used to block the current thread.
fn wait(vm_id: usize) {
    VM_VCPU_TASK_WAIT_QUEUE
        .lock()
        .get(&vm_id)
        .unwrap()
        .wait_queue
        .wait();
}

/// Blocks the current thread until the provided condition is met, using the wait queue
/// associated with the vCPUs of the specified VM.
///
/// # Arguments
///
/// * `vm_id` - The ID of the VM whose vCPU wait queue is used to block the current thread.
/// * `condition` - A closure that returns a boolean value indicating whether the condition is met.
fn wait_for<F>(vm_id: usize, condition: F)
where
    F: Fn() -> bool,
{
    VM_VCPU_TASK_WAIT_QUEUE
        .lock()
        .get(&vm_id)
        .unwrap()
        .wait_queue
        .wait_until(condition);
}

/// Sets up the vCPUs for a given VM by spawing `axtask` for each vCPU,
/// and initializing their respective wait queues and task lists.
///
/// # Arguments
///
/// * `vm` - A reference to the VM for which the vCPUs are being set up.
pub fn setup_vm_vcpus(vm: VMRef) {
    info!("Initializing VM[{}]'s {} vcpus", vm.id(), vm.vcpu_num());
    let vm_id = vm.id();

    VM_VCPU_TASK_WAIT_QUEUE
        .lock()
        .insert(vm_id, VMVcpus::new(vm.clone()));

    for vcpu in vm.vcpu_list() {
        info!("Spawning task for Vcpu[{}]", vcpu.id());
        let mut task = TaskInner::new(
            || {
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
                            AxVCpuExitReason::SystemDown => {
                                warn!("VM[{}] run VCpu[{}] SystemDown", vm_id, vcpu_id);
                                axhal::misc::terminate()
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
            },
            format!("VCpu[{}]", vcpu.id()),
            KERNEL_STACK_SIZE,
        );

        task.init_task_ext(TaskExt::new(vm.clone(), vcpu.clone()));
        let task_ref = axtask::spawn_task(task);

        VM_VCPU_TASK_WAIT_QUEUE
            .lock()
            .get_mut(&vm_id)
            .unwrap()
            .add_vcpu_task(task_ref);
    }
}
