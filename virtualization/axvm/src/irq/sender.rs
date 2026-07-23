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

//! Stable send channel from architecture interrupt routers to the VM runtime.
//!
//! The sender holds only a [`Weak`] reference to the VM, so it never creates
//! a strong reference cycle and automatically fails when the VM is destroyed.

use alloc::{
    format,
    sync::{Arc, Weak},
};

use crate::{AxVM, AxVmResult, VmStatus, ax_err, ax_err_type, irq::model::PendingVcpuInterrupt};

/// Router-to-runtime stable send channel.
///
/// Holds only a `Weak<AxVM>` — it never caches a runtime or dispatcher
/// reference. Every [`send`](Self::send) call looks up the current runtime
/// through the VM, so a VM stop/start/reset cycle cannot leave the sender
/// pointing at a stale dispatcher.
#[allow(dead_code, reason = "routers create senders in module 2+")]
#[derive(Clone)]
pub struct VmInterruptSender {
    vm: Weak<AxVM>,
}

impl VmInterruptSender {
    /// Constructs a sender from an `AxVMRef` (`Arc<AxVM>`).
    #[allow(dead_code, reason = "routers create senders in module 2+")]
    pub fn new(vm: &Arc<AxVM>) -> Self {
        Self {
            vm: Arc::downgrade(vm),
        }
    }

    /// Delivers a pending interrupt to the target vCPU through the current
    /// runtime dispatcher.
    ///
    /// # Flow
    ///
    /// 1. Upgrade `Weak<AxVM>` — returns `NotFound` if the VM has been
    ///    destroyed.
    /// 2. Check VM status — only `Running` and `Paused` accept interrupts;
    ///    other states return `BadState`.
    /// 3. `vm.with_runtime(|rt| rt.dispatch_vcpu_interrupt(vcpu_id,
    ///    interrupt))` — runtime not yet created returns `BadState`;
    ///    unregistered vCPU task returns `NotFound`.
    ///
    /// A TOCTOU window exists between steps 2 and 3 (status may change), but
    /// this matches the existing `vcpus::queue_interrupt` behaviour and is an
    /// acceptable window.
    #[allow(dead_code, reason = "interrupt routers will call send")]
    pub fn send(&self, vcpu_id: usize, interrupt: PendingVcpuInterrupt) -> AxVmResult {
        let vm = self
            .vm
            .upgrade()
            .ok_or_else(|| ax_err_type!(NotFound, "VM has been destroyed"))?;

        match vm.status() {
            VmStatus::Running | VmStatus::Paused => {}
            status => {
                return ax_err!(
                    BadState,
                    format!("VM[{}] cannot accept interrupts in {status:?}", vm.id())
                );
            }
        }

        vm.with_runtime(|rt| rt.dispatch_vcpu_interrupt(vcpu_id, interrupt))
    }
}
