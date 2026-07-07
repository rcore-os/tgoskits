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

use crate::vmm::{
    interrupt::{VirtualInterrupt, deliver_vcpu_interrupt},
    vm_list::get_vm_by_id,
};

pub fn hardware_check() {}

pub fn inject_current_interrupt(vector: u8) {
    let context = crate::context::current_vcpu_context();
    inject_interrupt(context.vm_id, context.vcpu_id, vector);
}

pub fn inject_interrupt(vm_id: usize, vcpu_id: usize, vector: u8) {
    debug!(
        "Injecting x86_64 virtual interrupt: vm_id={vm_id}, vcpu_id={vcpu_id}, vector={vector:#x}"
    );
    let Some(vm) = get_vm_by_id(vm_id) else {
        warn!("Failed to inject x86_64 interrupt: VM[{vm_id}] not found");
        return;
    };
    if let Err(err) = deliver_vcpu_interrupt(&vm, vcpu_id, VirtualInterrupt::edge(vector as usize))
    {
        warn!("Failed to inject x86_64 interrupt to VM[{vm_id}] VCpu[{vcpu_id}]: {err:?}");
    }
}
