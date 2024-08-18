use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use spin::Mutex;

use crate::vmm::VMRef;

/// Represents a list of VMs,
/// stored in a BTreeMap where the key is the VM ID and the value is a reference to the VM.
struct VMList {
    vm_list: BTreeMap<usize, VMRef>,
}

impl VMList {
    /// Creates a new, empty `VMList`.
    const fn new() -> VMList {
        VMList {
            vm_list: BTreeMap::new(),
        }
    }

    /// Adds a new VM to the list.
    ///
    /// If a VM with the given ID already exists, a warning is logged, and the VM is not added.
    ///
    /// # Arguments
    ///
    /// * `vm_id` - The unique identifier for the VM.
    /// * `vm` - A reference to the VM that will be added.
    fn push_vm(&mut self, vm_id: usize, vm: VMRef) {
        if self.vm_list.contains_key(&vm_id) {
            warn!(
                "VM[{}] already exists, push VM failed, just return ...",
                vm_id
            );
            return;
        }
        self.vm_list.insert(vm_id, vm);
    }

    /// Removes a VM from the list by its ID.
    ///
    /// # Arguments
    ///
    /// * `vm_id` - The unique identifier of the VM to be removed.
    ///
    /// # Returns
    ///
    /// Returns `Some(VMRef)` if the VM was successfully removed, or `None` if the VM with the given ID did not exist.
    #[allow(unused)]
    fn remove_vm(&mut self, vm_id: usize) -> Option<VMRef> {
        self.vm_list.remove(&vm_id)
    }

    /// Retrieves a VM from the list by its ID.
    ///
    /// # Arguments
    ///
    /// * `vm_id` - The unique identifier of the VM to be retrieved.
    ///
    /// # Returns
    ///
    /// Returns `Some(VMRef)` if the VM exists, or `None` if the VM with the given ID does not exist.
    fn get_vm_by_id(&self, vm_id: usize) -> Option<VMRef> {
        self.vm_list.get(&vm_id).cloned()
    }
}

// A global list of VMs, protected by a mutex for thread-safe access.
static GLOBAL_VM_LIST: Mutex<VMList> = Mutex::new(VMList::new());

/// Adds a VM to the global VM list.
///
/// # Arguments
///
/// * `vm` - A reference to the VM instance.
pub fn push_vm(vm: VMRef) {
    GLOBAL_VM_LIST.lock().push_vm(vm.id(), vm)
}

/// Removes a VM from the global VM list by its ID.
///
/// # Arguments
///
/// * `vm_id` - The unique identifier of the VM to be removed.
///
/// # Returns
///
/// * `Option<VMRef>` - The removed VM reference if it exists, or `None` if not.
#[allow(unused)]
pub fn remove_vm(vm_id: usize) -> Option<VMRef> {
    GLOBAL_VM_LIST.lock().remove_vm(vm_id)
}

/// Retrieves a VM from the global VM list by its ID.
///
/// # Arguments
///
/// * `vm_id` - The unique identifier of the VM to retrieve.
///
/// # Returns
///
/// * `Option<VMRef>` - The VM reference if it exists, or `None` if not.
#[allow(unused)]
pub fn get_vm_by_id(vm_id: usize) -> Option<VMRef> {
    GLOBAL_VM_LIST.lock().get_vm_by_id(vm_id)
}

pub fn get_vm_list() -> Vec<VMRef> {
    let global_vm_list = GLOBAL_VM_LIST.lock().vm_list.clone();
    let mut vm_list = Vec::with_capacity(global_vm_list.len());
    for (_id, vm) in global_vm_list {
        vm_list.push(vm.clone());
    }
    vm_list
}
