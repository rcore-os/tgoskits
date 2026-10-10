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

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axdevice_base::{HostIrqId, InterruptControllerId, InterruptTriggerMode};

use crate::identity::{RunId, VcpuInstance};

/// Identifies one activation of a VM run.
///
/// The run identity prevents an interrupt producer retired with an old run
/// from publishing into a later reset. `generation` is deliberately carried
/// separately from [`RunId`] so a controller can reject a stale activation
/// without consulting the VM lifecycle owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct RunEpoch {
    pub run: RunId,
    pub generation: u64,
}

impl RunEpoch {
    /// Creates the initial epoch for a run.
    pub const fn new(run: RunId) -> Self {
        Self {
            run,
            generation: run.generation(),
        }
    }

    /// Returns the run this epoch belongs to.
    pub const fn run(self) -> RunId {
        self.run
    }

    /// Returns the monotonic activation generation.
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Identifies a physical or virtual interrupt source without collapsing it to
/// a guest vector. Physical identity is required for ACK/EOI and teardown.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct InterruptSourceId {
    pub controller: InterruptControllerId,
    pub source: u32,
    pub physical: Option<HostIrqId>,
}

impl InterruptSourceId {
    /// Creates one source identity owned by an interrupt controller.
    pub const fn new(
        controller: InterruptControllerId,
        source: u32,
        physical: Option<HostIrqId>,
    ) -> Self {
        Self {
            controller,
            source,
            physical,
        }
    }
}

/// Identifies one delivery until its guest EOI or retirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeliveryToken {
    pub source: InterruptSourceId,
    pub target: VcpuInstance,
    pub sequence: u64,
}

/// A source event submitted by a device, host IRQ, or guest EOI path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceEvent {
    /// Delivers one edge to the shared controller owner.
    Pulse {
        epoch: RunEpoch,
        source: InterruptSourceId,
    },
    /// Changes one source's electrical level.
    Level {
        epoch: RunEpoch,
        source: InterruptSourceId,
        asserted: bool,
    },
    /// Completes a delivery and preserves source identity for redelivery.
    Eoi {
        epoch: RunEpoch,
        token: DeliveryToken,
    },
}

/// Owner-only interface for CPU-local interrupt state.
pub trait VcpuLocalInterrupts {
    /// Snapshot loaded by the architecture entry path.
    type Snapshot;
    /// Completion captured after the hardware state is saved.
    type Completion;
    /// Error returned by the architecture backend.
    type Error;

    /// Prepares local interrupt state for guest entry.
    fn prepare_entry(&mut self) -> Result<Self::Snapshot, Self::Error>;
    /// Injects one pending source into this vCPU's local state.
    fn inject(&mut self, interrupt: PendingVcpuInterrupt) -> Result<(), Self::Error>;
    /// Applies one guest EOI to the local state.
    fn handle_eoi(&mut self, token: DeliveryToken) -> Result<Self::Completion, Self::Error>;
    /// Saves local state after guest execution.
    fn save_exit(&mut self) -> Result<Self::Completion, Self::Error>;
    /// Resets local state for a new run epoch.
    fn reset(&mut self);
}

/// Owner-facing endpoint for one shared peripheral interrupt controller.
pub trait InterruptControllerEndpoint: Send + Sync {
    /// Error returned while publishing an event to the controller owner.
    type Error;

    /// Returns the VM-local controller identity.
    fn id(&self) -> InterruptControllerId;
    /// Submits an event through the fixed owner mailbox/ingress.
    fn submit(&self, event: SourceEvent) -> Result<(), Self::Error>;
}

/// Owner-only interface for a CPU-local guest timer.
pub trait VcpuLocalTimer {
    /// Error returned by the architecture timer backend.
    type Error;

    /// Arms the local timer at an architecture-defined deadline.
    fn arm(&mut self, deadline: u64) -> Result<(), Self::Error>;
    /// Quiesces the timer while preserving guest-visible state.
    fn suspend(&mut self) -> Result<(), Self::Error>;
    /// Resumes a suspended timer.
    fn resume(&mut self) -> Result<(), Self::Error>;
    /// Cancels the timer and retires its host callback.
    fn cancel(&mut self) -> Result<(), Self::Error>;
    /// Consumes one or more expiries published by the host ingress.
    fn consume_expiry(&mut self) -> bool;
}

/// Fixed atomic ingress for one vCPU-owned host timer.
///
/// The callback side only calls [`Self::publish_expiry`]. It never obtains a
/// vCPU, device, or VM lock. The vCPU owner consumes the counter after it has
/// returned from hardware execution.
pub struct VcpuTimerIngress {
    pending: AtomicU64,
    generation: AtomicU64,
    accepting: AtomicBool,
}

impl VcpuTimerIngress {
    /// Creates a closed timer ingress with no pending expiries.
    pub const fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            accepting: AtomicBool::new(false),
        }
    }

    /// Opens a new timer activation and returns its generation.
    pub fn arm(&self) -> u64 {
        let generation = self.next_generation();
        self.pending.store(0, Ordering::Release);
        self.accepting.store(true, Ordering::Release);
        generation
    }

    /// Closes the ingress and retires callbacks from the current generation.
    pub fn close(&self) {
        self.accepting.store(false, Ordering::Release);
        let _ = self.next_generation();
    }

    /// Publishes one expiry from a host callback.
    pub fn publish_expiry(&self, generation: u64) -> bool {
        if !self.accepting.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != generation
        {
            return false;
        }
        self.pending.fetch_add(1, Ordering::Release);
        true
    }

    /// Consumes all expiries for the expected generation.
    pub fn take_expiries(&self, generation: u64) -> u64 {
        if self.generation.load(Ordering::Acquire) != generation {
            return 0;
        }
        self.pending.swap(0, Ordering::AcqRel)
    }

    fn next_generation(&self) -> u64 {
        self.generation
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .unwrap_or_else(|_| panic!("vCPU timer generation exhausted"))
            + 1
    }
}

/// Architecture-independent virtual interrupt identifier.
///
/// Uses `u32` to avoid leaking x86 `u8` vector limits into GIC (INTID up to 1020+),
/// PLIC, and LoongArch.
///
/// Will be constructed by architecture interrupt routers and consumed by
/// the run-bound signal endpoint when a virtual
/// device raises an interrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct VirtualInterruptId(pub u32);

/// An interrupt event pending delivery to a target vCPU.
///
/// Carries the trigger mode (edge/level) so that architecture injection paths
/// and routers can preserve the semantics declared by the device.
///
/// Will be enqueued into a run-bound fixed signal slot
/// and later drained by the target vCPU run loop for injection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingVcpuInterrupt {
    pub id: VirtualInterruptId,
    pub trigger: InterruptTriggerMode,
    /// Optional physical/controller source retained until EOI or retirement.
    ///
    /// Existing guest-vector-only producers use `None`; architecture ingress
    /// paths that need ACK/EOI or level redelivery attach the source identity.
    pub source: Option<InterruptSourceId>,
}

impl Default for VcpuTimerIngress {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::VmKey;

    fn run() -> RunId {
        RunId::new(VmKey::new(1, 1), 1)
    }

    #[test]
    fn timer_ingress_counts_edges_and_rejects_stale_callbacks() {
        let ingress = VcpuTimerIngress::new();
        let old = ingress.arm();
        assert!(ingress.publish_expiry(old));
        assert!(ingress.publish_expiry(old));
        assert_eq!(ingress.take_expiries(old), 2);

        ingress.close();
        let current = ingress.arm();
        assert!(!ingress.publish_expiry(old));
        assert!(ingress.publish_expiry(current));
        assert_eq!(ingress.take_expiries(current), 1);
    }

    #[test]
    fn source_event_keeps_epoch_and_physical_identity() {
        let epoch = RunEpoch::new(run());
        let source =
            InterruptSourceId::new(InterruptControllerId::new(2), 32, Some(HostIrqId::new(47)));
        let target = VcpuInstance {
            run: epoch.run,
            vcpu_id: 0,
            activation: 3,
        };
        let token = DeliveryToken {
            source,
            target,
            sequence: 9,
        };
        let event = SourceEvent::Eoi { epoch, token };
        assert!(matches!(
            event,
            SourceEvent::Eoi { epoch: actual, token: actual_token }
                if actual == epoch && actual_token == token
        ));
    }
}
