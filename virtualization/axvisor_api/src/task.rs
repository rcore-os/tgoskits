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
}

/// Options used when spawning a host task.
pub struct TaskOptions {
    /// Human-readable task name.
    pub name: String,
    /// Requested kernel stack size in bytes.
    pub stack_size: usize,
    /// Optional host CPU affinity mask.
    pub cpu_set: Option<usize>,
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

/// Spawns a host task.
pub fn spawn_task<F>(options: TaskOptions, entry: F) -> TaskHandle
where
    F: FnOnce() + Send + 'static,
{
    spawn_task_raw(options, Box::new(entry))
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

    /// Spawns a host task.
    fn spawn_task_raw(
        options: TaskOptions,
        entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> TaskHandle;

    /// Waits for the task to exit.
    fn join_task(task: TaskHandle);

    /// Returns the current host task, if execution is in a task context.
    fn current_task() -> Option<TaskHandle>;

    /// Yield the current host task/thread.
    fn yield_now();
}
