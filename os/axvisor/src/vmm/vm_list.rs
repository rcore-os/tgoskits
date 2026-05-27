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

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use ax_kspin::SpinNoIrq as Mutex;

use crate::vmm::VMRef;

// A global map of VMs, indexed by VM ID, protected by a mutex for thread-safe access.
static GLOBAL_VM_LIST: Mutex<BTreeMap<usize, VMRef>> = Mutex::new(BTreeMap::new());

/// Adds a VM to the global VM list.
///
/// If a VM with the same ID already exists, a warning is logged and the VM is not added.
pub fn push_vm(vm: VMRef) {
    let vm_id = vm.id();
    let mut list = GLOBAL_VM_LIST.lock();
    if list.contains_key(&vm_id) {
        warn!("VM[{vm_id}] already exists, push VM failed, just return ...");
        return;
    }
    list.insert(vm_id, vm);
}

/// Removes a VM from the global VM list by its ID.
#[allow(unused)]
pub fn remove_vm(vm_id: usize) -> Option<VMRef> {
    GLOBAL_VM_LIST.lock().remove(&vm_id)
}

/// Retrieves a VM from the global VM list by its ID.
#[allow(unused)]
pub fn get_vm_by_id(vm_id: usize) -> Option<VMRef> {
    GLOBAL_VM_LIST.lock().get(&vm_id).cloned()
}

pub fn get_vm_list() -> Vec<VMRef> {
    GLOBAL_VM_LIST.lock().values().cloned().collect()
}
