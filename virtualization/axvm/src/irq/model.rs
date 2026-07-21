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

//! Architecture-independent virtual interrupt model types.

use axdevice_base::InterruptTriggerMode;

/// Architecture-independent virtual interrupt identifier.
///
/// Uses `u32` to avoid leaking x86 `u8` vector limits into GIC (INTID up to 1020+),
/// PLIC, and LoongArch.
///
/// Will be constructed by architecture interrupt routers and consumed by
/// [`VcpuIrqDispatcher`](crate::runtime::VcpuIrqDispatcher) when a virtual
/// device raises an interrupt.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct VirtualInterruptId(pub u32);

/// An interrupt event pending delivery to a target vCPU.
///
/// Carries the trigger mode (edge/level) so that architecture injection paths
/// and routers can preserve the semantics declared by the device.
///
/// Will be enqueued into [`VcpuIrqDispatcher`](crate::runtime::VcpuIrqDispatcher)
/// and later drained by the target vCPU run loop for injection.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingVcpuInterrupt {
    pub id: VirtualInterruptId,
    pub trigger: InterruptTriggerMode,
}
