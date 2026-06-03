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

use axvisor_api::vmm::inject_interrupt as inject_vcpu_interrupt;

pub fn hardware_check() {}

pub fn inject_current_interrupt(vector: u8) {
    let context = crate::context::current_vcpu_context();
    inject_interrupt(context.vm_id, context.vcpu_id, vector);
}

pub fn inject_interrupt(vm_id: usize, vcpu_id: usize, vector: u8) {
    debug!(
        "Injecting x86_64 virtual interrupt: vm_id={vm_id}, vcpu_id={vcpu_id}, vector={vector:#x}"
    );
    inject_vcpu_interrupt(vm_id, vcpu_id, vector);
}
