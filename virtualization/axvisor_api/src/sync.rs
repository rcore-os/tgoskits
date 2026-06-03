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

//! Host blocking synchronization APIs for AxVisor.

extern crate alloc;

use alloc::boxed::Box;

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

    /// Wakes one waiter, if any.
    pub fn wake_one(&self) {
        wait_queue_wake_one(self.raw);
    }

    /// Wakes all waiters.
    pub fn wake_all(&self) {
        wait_queue_wake_all(self.raw);
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

/// The host blocking synchronization API required by AxVisor.
#[crate::api_def]
pub trait SyncIf {
    /// Allocates a new host wait queue.
    fn create_wait_queue() -> usize;

    /// Destroys a previously allocated host wait queue.
    fn destroy_wait_queue(queue: usize);

    /// Blocks the current host task on the specified wait queue.
    fn wait_queue_wait(queue: usize);

    /// Blocks the current host task until `condition` becomes true.
    fn wait_queue_wait_until(queue: usize, condition: Box<dyn Fn() -> bool + Send + 'static>);

    /// Wakes one task blocked on the specified wait queue.
    fn wait_queue_wake_one(queue: usize);

    /// Wakes all tasks blocked on the specified wait queue.
    fn wait_queue_wake_all(queue: usize);
}
