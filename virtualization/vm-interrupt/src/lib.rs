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

//! Shared virtual interrupt routing model.
//!
//! This crate intentionally contains only data types and small routing traits.
//! It does not know how a hypervisor schedules vCPUs, whether a VM is static or
//! host-controlled, or how an architecture backend injects an event. Those
//! policy decisions belong to the VMM using these types.

#![no_std]

use ax_errno::AxResult;

/// Edge/level trigger metadata for interrupt backends that distinguish it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptTriggerMode {
    /// Edge-triggered interrupt.
    EdgeTriggered,
    /// Level-triggered interrupt.
    LevelTriggered,
}

/// The desired level of a virtual interrupt line.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InterruptLineLevel {
    /// Assert the interrupt line for the target vCPU.
    Assert,
    /// Deassert the interrupt line for the target vCPU.
    Deassert,
}

/// A virtual interrupt event after interrupt-controller routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualInterrupt {
    /// Architecture-specific virtual vector or interrupt cause.
    pub vector: usize,
    /// Edge/level metadata used by architectures that distinguish trigger mode.
    pub trigger: InterruptTriggerMode,
    /// Whether the virtual line is being asserted or deasserted.
    pub level: InterruptLineLevel,
}

impl VirtualInterrupt {
    /// Construct an asserted edge-triggered vector interrupt.
    pub const fn edge(vector: usize) -> Self {
        Self::with_trigger(vector, InterruptTriggerMode::EdgeTriggered)
    }

    /// Construct an asserted vector interrupt with explicit trigger metadata.
    pub const fn with_trigger(vector: usize, trigger: InterruptTriggerMode) -> Self {
        Self {
            vector,
            trigger,
            level: InterruptLineLevel::Assert,
        }
    }

    /// Construct a line deassertion event.
    pub const fn deassert(vector: usize) -> Self {
        Self {
            vector,
            trigger: InterruptTriggerMode::EdgeTriggered,
            level: InterruptLineLevel::Deassert,
        }
    }
}

/// A routed interrupt whose target VM and vCPU are already known.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptRoute {
    /// Target VM ID.
    pub vm_id: usize,
    /// Target vCPU ID, local to the VM.
    pub vcpu_id: usize,
    /// Interrupt event to deliver.
    pub interrupt: VirtualInterrupt,
}

impl InterruptRoute {
    /// Construct a VM-global interrupt route.
    pub const fn new(vm_id: usize, vcpu_id: usize, interrupt: VirtualInterrupt) -> Self {
        Self {
            vm_id,
            vcpu_id,
            interrupt,
        }
    }
}

/// A guest-local set of target vCPUs for an interrupt event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VcpuInterruptTarget {
    /// One vCPU by VM-local vCPU ID.
    Vcpu(usize),
    /// One vCPU by guest-visible CPU topology ID.
    GuestCpu(usize),
    /// Every vCPU in the VM, optionally including the sender.
    All {
        /// Sender vCPU ID.
        current_vcpu_id: usize,
        /// Whether the sender is included in the broadcast.
        include_current: bool,
    },
    /// Guest CPU/hart mask.
    ///
    /// The mask is expressed in guest-visible CPU topology IDs. The VMM that
    /// owns the VM must translate those IDs to vCPU IDs.
    GuestCpuMask {
        /// Bit mask of target guest CPU IDs.
        mask: usize,
        /// Base guest CPU ID for bit 0, or `usize::MAX` for all CPUs.
        base: usize,
    },
}

/// A routed interrupt emitted by an interrupt controller inside its owning VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterruptControllerRoute {
    /// Target vCPU ID, local to the VM that owns the device.
    pub vcpu_id: usize,
    /// Interrupt event to route.
    pub interrupt: VirtualInterrupt,
}

impl InterruptControllerRoute {
    /// Construct a VM-local interrupt-controller route.
    pub const fn new(vcpu_id: usize, interrupt: VirtualInterrupt) -> Self {
        Self { vcpu_id, interrupt }
    }
}

/// Routes device interrupts to vCPUs of the VM that owns the device.
///
/// Device models use this trait to report fully routed, guest-local interrupt
/// events without depending on a global VM registry or knowing the owner VM ID.
pub trait VmInterruptRouter: Send + Sync {
    /// Route one interrupt event to the owning VM.
    fn route_interrupt(&self, route: InterruptControllerRoute) -> AxResult;
}
