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

//! Host tasking and wait-queue APIs for AxVisor.

extern crate alloc;

use alloc::{boxed::Box, string::String};

use crate::types::{VCpuId, VMId};

/// An opaque host task handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct TaskHandle(usize);

impl TaskHandle {
    /// Creates a task handle from a raw host-provided identifier.
    pub const fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    /// Returns the raw host-provided identifier.
    pub const fn as_raw(self) -> usize {
        self.0
    }

    /// Returns a human-readable identifier for the task.
    pub fn id_name(self) -> String {
        task_id_name(self)
    }

    /// Returns the host CPU on which the task is running or queued.
    pub fn cpu_id(self) -> usize {
        task_cpu_id(self)
    }

    /// Waits for the task to exit and returns its exit code.
    pub fn join(self) -> i32 {
        task_join(self)
    }
}

/// An opaque wait queue handle allocated by the host runtime.
#[derive(Debug)]
pub struct WaitQueue {
    raw: usize,
}

impl WaitQueue {
    /// Creates a new host wait queue.
    pub fn new() -> Self {
        Self {
            raw: create_wait_queue(),
        }
    }

    /// Returns the raw host-provided identifier.
    pub const fn as_raw(&self) -> usize {
        self.raw
    }

    /// Blocks the current task until it is explicitly woken.
    pub fn wait(&self) {
        wait_queue_wait(self.raw);
    }

    /// Blocks until `condition` returns true.
    pub fn wait_until<F>(&self, condition: F)
    where
        F: Fn() -> bool + Send + 'static,
    {
        wait_queue_wait_until(self.raw, Box::new(condition));
    }

    /// Wakes up to `count` waiters.
    pub fn wake(&self, count: u32) {
        wait_queue_wake(self.raw, count);
    }

    /// Wakes one waiter, if any.
    pub fn wake_one(&self) {
        self.wake(1);
    }

    /// Wakes all waiters.
    pub fn wake_all(&self) {
        self.wake(u32::MAX);
    }
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WaitQueue {
    fn drop(&mut self) {
        destroy_wait_queue(self.raw);
    }
}

/// Spawns a host task that will execute the specified vCPU entry routine.
pub fn spawn_vcpu_task<F>(
    vm_id: VMId,
    vcpu_id: VCpuId,
    phys_cpu_set: Option<usize>,
    stack_size: usize,
    entry: F,
) -> TaskHandle
where
    F: FnOnce() + Send + 'static,
{
    spawn_vcpu_task_raw(vm_id, vcpu_id, phys_cpu_set, stack_size, Box::new(entry))
}

/// The host tasking API required by AxVisor.
#[crate::api_def]
pub trait TaskIf {
    /// Allocates a new host wait queue.
    fn create_wait_queue() -> usize;

    /// Destroys a previously allocated host wait queue.
    fn destroy_wait_queue(queue: usize);

    /// Blocks the current host task on the specified wait queue.
    fn wait_queue_wait(queue: usize);

    /// Blocks the current host task until `condition` becomes true.
    fn wait_queue_wait_until(queue: usize, condition: Box<dyn Fn() -> bool + Send + 'static>);

    /// Wakes up to `count` tasks blocked on the specified wait queue.
    fn wait_queue_wake(queue: usize, count: u32);

    /// Spawns a host task bound to a vCPU execution context.
    fn spawn_vcpu_task_raw(
        vm_id: VMId,
        vcpu_id: VCpuId,
        phys_cpu_set: Option<usize>,
        stack_size: usize,
        entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> TaskHandle;

    /// Returns a human-readable identifier for the task.
    fn task_id_name(task: TaskHandle) -> String;

    /// Returns the host CPU on which the task is running or queued.
    fn task_cpu_id(task: TaskHandle) -> usize;

    /// Waits for the task to exit and returns its exit code.
    fn task_join(task: TaskHandle) -> i32;

    /// Returns the VM ID bound to the current vCPU host task.
    fn current_vm_id() -> VMId;

    /// Returns the vCPU ID bound to the current vCPU host task.
    fn current_vcpu_id() -> VCpuId;
}
