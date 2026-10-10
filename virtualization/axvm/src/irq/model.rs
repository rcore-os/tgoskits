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

use core::sync::atomic::{AtomicU64, Ordering};

use axdevice_base::{
    ControllerInputId, HostIrqId, InterruptControllerId, InterruptTriggerMode, IrqResult,
    WiredIrqInput,
};

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
    /// Saves local state after guest execution.
    fn save_exit(&mut self) -> Result<Self::Completion, Self::Error>;
}

/// Owner-facing endpoint for one shared peripheral interrupt controller.
pub trait InterruptControllerEndpoint: Send + Sync {
    /// Error returned while publishing an event to the controller owner.
    type Error;

    /// Returns the VM-local controller identity.
    fn id(&self) -> InterruptControllerId;
    /// Opens a run-independent wired input. The returned input carries the
    /// source identity used by subsequent [`SourceEvent`] submissions.
    fn wired_input(
        &self,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
    ) -> IrqResult<WiredIrqInput>;
    /// Submits an event through the fixed owner mailbox/ingress.
    fn submit(&self, event: SourceEvent) -> Result<(), Self::Error>;
}

/// Task-owner side of a shared interrupt controller.
///
/// `submit` is the producer-facing ingress. `apply_source` is reachable only
/// from the lifecycle owner after the fixed ingress has been drained.
pub trait InterruptControllerOwner: Send + Sync {
    type Error;

    fn apply_source(&self, event: SourceEvent) -> Result<(), Self::Error>;
}

/// Owner-only interface for a CPU-local guest timer.
pub trait VcpuLocalTimer {
    /// Error returned by the architecture timer backend.
    type Error;

    /// Quiesces the timer while preserving guest-visible state.
    fn suspend(&mut self) -> Result<(), Self::Error>;
    /// Resumes a suspended timer.
    fn resume(&mut self) -> Result<(), Self::Error>;
    /// Cancels the timer and retires its host callback.
    fn cancel(&mut self) -> Result<(), Self::Error>;
}

/// Fixed atomic ingress for one vCPU-owned host timer.
///
/// The callback side only calls [`Self::publish_expiry`]. It never obtains a
/// vCPU, device, or VM lock. The vCPU owner consumes the counter after it has
/// returned from hardware execution.
pub struct VcpuTimerIngress {
    /// Bit 63 is admission, bits 32..62 are the generation, and the lower
    /// 32 bits count pending edges. One CAS word makes close/arm/publish
    /// linearizable without taking a lock in a host timer callback.
    state: AtomicU64,
}

impl VcpuTimerIngress {
    const ACCEPTING: u64 = 1 << 63;
    const PENDING_MASK: u64 = u32::MAX as u64;
    const GENERATION_MASK: u64 = (1 << 31) - 1;
    const GENERATION_SHIFT: u32 = 32;

    /// Creates a closed timer ingress with no pending expiries.
    pub const fn new() -> Self {
        Self {
            state: AtomicU64::new(0),
        }
    }

    /// Opens a new timer activation and returns its generation.
    ///
    /// Once the bounded generation space is exhausted, the ingress is closed
    /// and the caller must retire the timer instead of reusing a generation
    /// that an old callback could still carry.
    pub fn arm(&self) -> Option<u64> {
        self.transition(true)
    }

    /// Closes the ingress and retires callbacks from the current generation.
    pub fn close(&self) -> bool {
        self.transition(false).is_some()
    }

    /// Publishes one expiry from a host callback.
    pub fn publish_expiry(&self, generation: u64) -> bool {
        if generation > Self::GENERATION_MASK {
            return false;
        }
        let expected_generation = generation;
        loop {
            let current = self.state.load(Ordering::Acquire);
            if current & Self::ACCEPTING == 0 || Self::generation(current) != expected_generation {
                return false;
            }
            // A saturated counter still represents an accepted edge. The
            // owner will consume the saturated batch before the next timer
            // activation; treating saturation as a stale callback would stop
            // a valid periodic timer permanently.
            if current & Self::PENDING_MASK == Self::PENDING_MASK {
                return true;
            }
            if self
                .state
                .compare_exchange_weak(current, current + 1, Ordering::Release, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Consumes all expiries for the expected generation.
    pub fn take_expiries(&self, generation: u64) -> u64 {
        if generation > Self::GENERATION_MASK {
            return 0;
        }
        let expected_generation = generation;
        loop {
            let current = self.state.load(Ordering::Acquire);
            if Self::generation(current) != expected_generation {
                return 0;
            }
            let pending = current & Self::PENDING_MASK;
            let next = current & !Self::PENDING_MASK;
            if self
                .state
                .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return pending;
            }
        }
    }

    /// Consumes expiries from the currently published generation.
    pub fn take_current_expiries(&self) -> u64 {
        let generation = Self::generation(self.state.load(Ordering::Acquire));
        self.take_expiries(generation)
    }

    fn generation(state: u64) -> u64 {
        (state >> Self::GENERATION_SHIFT) & Self::GENERATION_MASK
    }

    fn transition(&self, accepting: bool) -> Option<u64> {
        loop {
            let current = self.state.load(Ordering::Acquire);
            let Some(generation) = Self::generation(current)
                .checked_add(1)
                .filter(|next| *next <= Self::GENERATION_MASK)
            else {
                // Do not reuse the last generation: an old callback carrying
                // it must remain stale forever. Closing admission is the only
                // safe recovery once the bounded identity space is exhausted.
                if self
                    .state
                    .compare_exchange_weak(
                        current,
                        current & !Self::ACCEPTING,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return None;
                }
                continue;
            };
            let next = (generation << Self::GENERATION_SHIFT)
                | if accepting { Self::ACCEPTING } else { 0 };
            if self
                .state
                .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(generation);
            }
        }
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
        let old = ingress.arm().expect("initial generation must be available");
        assert!(ingress.publish_expiry(old));
        assert!(ingress.publish_expiry(old));
        assert_eq!(ingress.take_expiries(old), 2);

        ingress.close();
        let current = ingress.arm().expect("second generation must be available");
        assert!(!ingress.publish_expiry(old));
        assert!(!ingress.publish_expiry(VcpuTimerIngress::GENERATION_MASK + 1));
        assert_eq!(
            ingress.take_expiries(VcpuTimerIngress::GENERATION_MASK + 1),
            0
        );
        assert!(ingress.publish_expiry(current));
        assert_eq!(ingress.take_expiries(current), 1);
    }

    #[test]
    fn timer_ingress_saturation_keeps_the_callback_live() {
        let ingress = VcpuTimerIngress::new();
        let generation = ingress.arm().expect("generation must be available");
        ingress.state.store(
            (generation << VcpuTimerIngress::GENERATION_SHIFT)
                | VcpuTimerIngress::ACCEPTING
                | VcpuTimerIngress::PENDING_MASK,
            Ordering::Release,
        );

        assert!(ingress.publish_expiry(generation));
        assert_eq!(
            ingress.take_expiries(generation),
            VcpuTimerIngress::PENDING_MASK
        );
    }

    #[test]
    fn timer_ingress_exhaustion_closes_without_panicking() {
        let ingress = VcpuTimerIngress::new();
        ingress.state.store(
            (VcpuTimerIngress::GENERATION_MASK << VcpuTimerIngress::GENERATION_SHIFT)
                | VcpuTimerIngress::ACCEPTING,
            Ordering::Release,
        );

        assert_eq!(ingress.arm(), None);
        assert!(!ingress.publish_expiry(VcpuTimerIngress::GENERATION_MASK));
        assert_eq!(ingress.take_current_expiries(), 0);
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
