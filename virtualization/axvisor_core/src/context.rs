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

use ax_kspin::SpinNoIrq;
use axvisor_api::{
    task::TaskHandle,
    types::{VCpuId, VMId},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VcpuContext {
    pub vm_id: VMId,
    pub vcpu_id: VCpuId,
}

static VCPU_CONTEXTS: SpinNoIrq<BTreeMap<TaskHandle, VcpuContext>> =
    SpinNoIrq::new(BTreeMap::new());

pub(crate) struct VcpuContextGuard {
    task: TaskHandle,
}

impl Drop for VcpuContextGuard {
    fn drop(&mut self) {
        VCPU_CONTEXTS.lock().remove(&self.task);
    }
}

pub(crate) fn bind_current_vcpu_context(vm_id: VMId, vcpu_id: VCpuId) -> VcpuContextGuard {
    let task = axvisor_api::task::current_task()
        .expect("current vCPU context cannot be bound outside of a host task");
    VCPU_CONTEXTS
        .lock()
        .insert(task, VcpuContext { vm_id, vcpu_id });
    VcpuContextGuard { task }
}

pub fn try_current_vcpu_context() -> Option<VcpuContext> {
    let task = axvisor_api::task::current_task()?;
    VCPU_CONTEXTS.lock().get(&task).copied()
}

pub fn current_vcpu_context() -> VcpuContext {
    try_current_vcpu_context().expect("current vCPU context requested outside of a vCPU task")
}

pub fn current_vm_id() -> VMId {
    current_vcpu_context().vm_id
}

pub fn current_vcpu_id() -> VCpuId {
    current_vcpu_context().vcpu_id
}
