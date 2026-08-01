// Copyright 2026 The Axvisor Team
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

// Runtime ownership gate for emulated physical SPIs.

use alloc::vec::Vec;

use ax_kspin::SpinNoIrq as Mutex;

use crate::{AxVmError, AxVmResult};

const FIRST_SPI_INTID: u32 = 32;
const RESERVED_SPI_INTID: u32 = 1020;

/// One emulated SPI assigned to a passthrough vCPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PassthroughSpiRegistration {
    vcpu_id: usize,
    intid: usize,
    target_mpidr: usize,
}

impl PassthroughSpiRegistration {
    pub(crate) const fn new(vcpu_id: usize, intid: usize, target_mpidr: usize) -> Self {
        Self {
            vcpu_id,
            intid,
            target_mpidr,
        }
    }
}

/// Whether a delivery can change the distributor route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PhysicalSpiRoutePolicy {
    /// The gate has proved that the SPI is not active.
    Configure,
    /// The SPI can still be active and its route must remain unchanged.
    Preserve,
}

/// One physical SPI delivery prepared by the ownership gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PhysicalSpiDelivery {
    pub(crate) intid: usize,
    pub(crate) target_mpidr: usize,
    pub(crate) route_policy: PhysicalSpiRoutePolicy,
}

/// Physical distributor state observed immediately after guest exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PhysicalSpiState {
    pub(crate) active: bool,
    pub(crate) pending: bool,
}

/// One preallocated request/result slot for guest-exit reclamation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PhysicalSpiReclaim {
    slot_index: usize,
    pub(crate) intid: usize,
    pub(crate) state: Option<PhysicalSpiState>,
}

impl PhysicalSpiReclaim {
    const fn new(slot_index: usize, intid: usize) -> Self {
        Self {
            slot_index,
            intid,
            state: None,
        }
    }
}

/// Physical interrupt-controller capability required by the gate.
pub(crate) trait PassthroughSpiController {
    /// Makes a complete delivery batch pending after all route work succeeds.
    fn deliver_spis(&mut self, requests: &[PhysicalSpiDelivery]) -> AxVmResult;

    /// Observes and clears pending state for a complete exit batch.
    fn reclaim_spis(&mut self, requests: &mut [PhysicalSpiReclaim]) -> AxVmResult;
}

/// Result of publishing an emulated SPI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PassthroughSpiSignal {
    /// The target vCPU is outside the guest and must be woken.
    Queued,
    /// The target vCPU owns the physical CPU interface and was signaled directly.
    Delivered,
}

/// Parameters for publishing one emulated-device SPI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PassthroughSpiSignalRequest {
    pub(crate) irq: usize,
    pub(crate) target_mpidr: usize,
}

/// The physical CPU-interface owner selected by an ownership transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PassthroughInterfaceOwner {
    Host,
    Guest,
}

/// One serialized gate operation: `Continue` publishes an SPI and `Break` transfers ownership.
pub(crate) type PassthroughSpiTransition =
    core::ops::ControlFlow<PassthroughInterfaceOwner, PassthroughSpiSignalRequest>;

/// Result produced by a physical-SPI ownership transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PassthroughSpiTransitionResult {
    /// Result of publishing one SPI.
    Signal(PassthroughSpiSignal),
    /// An ownership transfer completed.
    OwnershipTransferred,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SpiSlot {
    intid: usize,
    target_mpidr: usize,
    queued: bool,
    armed: bool,
}

struct VcpuPassthroughState {
    owner: PassthroughInterfaceOwner,
    slots: Vec<SpiSlot>,
    delivery_batch: Vec<PhysicalSpiDelivery>,
    reclaim_batch: Vec<PhysicalSpiReclaim>,
}

impl VcpuPassthroughState {
    fn with_capacity(slot_count: usize) -> AxVmResult<Self> {
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(slot_count)
            .map_err(|_| AxVmError::OutOfMemory {
                operation: "preallocating passthrough SPI slots",
            })?;
        let mut delivery_batch = Vec::new();
        delivery_batch
            .try_reserve_exact(slot_count)
            .map_err(|_| AxVmError::OutOfMemory {
                operation: "preallocating passthrough SPI delivery batch",
            })?;
        let mut reclaim_batch = Vec::new();
        reclaim_batch
            .try_reserve_exact(slot_count)
            .map_err(|_| AxVmError::OutOfMemory {
                operation: "preallocating passthrough SPI reclaim batch",
            })?;
        Ok(Self {
            owner: PassthroughInterfaceOwner::Host,
            slots,
            delivery_batch,
            reclaim_batch,
        })
    }
}

/// Per-VM gate that serializes producer, entry, and exit transitions.
pub(crate) struct PassthroughSpiGate {
    vcpus: Mutex<Vec<VcpuPassthroughState>>,
}

impl PassthroughSpiGate {
    pub(crate) fn new(
        vcpu_count: usize,
        registrations: &[PassthroughSpiRegistration],
    ) -> AxVmResult<Self> {
        let mut slot_counts = Vec::new();
        slot_counts
            .try_reserve_exact(vcpu_count)
            .map_err(|_| AxVmError::OutOfMemory {
                operation: "counting passthrough SPI slots",
            })?;
        slot_counts.resize(vcpu_count, 0usize);

        for registration in registrations {
            let raw_intid = u32::try_from(registration.intid).map_err(|_| {
                AxVmError::invalid_config(format_args!(
                    "passthrough INTID {} does not fit in u32",
                    registration.intid
                ))
            })?;
            if !(FIRST_SPI_INTID..RESERVED_SPI_INTID).contains(&raw_intid) {
                return Err(AxVmError::invalid_config(format_args!(
                    "passthrough INTID {} is not an architectural SPI",
                    registration.intid
                )));
            }
            let count = slot_counts.get_mut(registration.vcpu_id).ok_or_else(|| {
                AxVmError::invalid_config(format_args!(
                    "passthrough INTID {} targets missing vCPU {}",
                    registration.intid, registration.vcpu_id
                ))
            })?;
            *count = count.checked_add(1).ok_or(AxVmError::OutOfMemory {
                operation: "counting passthrough SPI slots",
            })?;
        }

        let mut vcpus = Vec::new();
        vcpus
            .try_reserve_exact(vcpu_count)
            .map_err(|_| AxVmError::OutOfMemory {
                operation: "preallocating passthrough vCPU gates",
            })?;
        for slot_count in slot_counts {
            vcpus.push(VcpuPassthroughState::with_capacity(slot_count)?);
        }

        for registration in registrations {
            let state = &mut vcpus[registration.vcpu_id];
            if let Some(existing) = state
                .slots
                .iter()
                .find(|slot| slot.intid == registration.intid)
            {
                if existing.target_mpidr != registration.target_mpidr {
                    return Err(AxVmError::invalid_config(format_args!(
                        "passthrough INTID {} has conflicting MPIDR targets {:#x} and {:#x}",
                        registration.intid, existing.target_mpidr, registration.target_mpidr
                    )));
                }
                continue;
            }
            state.slots.push(SpiSlot {
                intid: registration.intid,
                target_mpidr: registration.target_mpidr,
                queued: false,
                armed: false,
            });
        }

        Ok(Self {
            vcpus: Mutex::new(vcpus),
        })
    }

    /// Publishes an emulated SPI without allocating or sending a host IPI.
    pub(crate) fn signal_passthrough_spi(
        &self,
        vcpu_id: usize,
        intid: usize,
        target_mpidr: usize,
        controller: &mut dyn PassthroughSpiController,
    ) -> AxVmResult<PassthroughSpiSignal> {
        let mut vcpus = self.vcpus.lock();
        let state = vcpus.get_mut(vcpu_id).ok_or_else(|| {
            AxVmError::invalid_input(
                "signal passthrough SPI",
                format_args!("vCPU {vcpu_id} has no runtime gate"),
            )
        })?;
        let owner = state.owner;
        let slot = state
            .slots
            .iter_mut()
            .find(|slot| slot.intid == intid)
            .ok_or_else(|| {
                AxVmError::invalid_input(
                    "signal passthrough SPI",
                    format_args!("INTID {intid} was not preallocated for vCPU {vcpu_id}"),
                )
            })?;

        if slot.target_mpidr != target_mpidr {
            return Err(AxVmError::resource_conflict(
                "passthrough SPI route",
                format_args!(
                    "INTID {intid} was preallocated for MPIDR {:#x}, not {target_mpidr:#x}",
                    slot.target_mpidr
                ),
            ));
        }

        if owner == PassthroughInterfaceOwner::Host {
            slot.queued = true;
            return Ok(PassthroughSpiSignal::Queued);
        }

        let request = PhysicalSpiDelivery {
            intid,
            target_mpidr,
            route_policy: if slot.armed {
                PhysicalSpiRoutePolicy::Preserve
            } else {
                PhysicalSpiRoutePolicy::Configure
            },
        };
        controller.deliver_spis(core::slice::from_ref(&request))?;
        slot.armed = true;
        Ok(PassthroughSpiSignal::Delivered)
    }

    /// Delivers queued SPIs and transfers interface ownership to the guest.
    pub(crate) fn prepare_guest_entry(
        &self,
        vcpu_id: usize,
        controller: &mut dyn PassthroughSpiController,
    ) -> AxVmResult {
        let mut vcpus = self.vcpus.lock();
        let state = vcpus.get_mut(vcpu_id).ok_or_else(|| {
            AxVmError::invalid_state(
                "prepare passthrough guest entry",
                format_args!("vCPU {vcpu_id} has no runtime gate"),
            )
        })?;
        if state.owner != PassthroughInterfaceOwner::Host {
            return Err(AxVmError::invalid_state(
                "prepare passthrough guest entry",
                format_args!("vCPU {vcpu_id} interface is already guest-owned"),
            ));
        }

        state.delivery_batch.clear();
        for slot in state.slots.iter().copied().filter(|slot| slot.queued) {
            debug_assert!(state.delivery_batch.len() < state.delivery_batch.capacity());
            state.delivery_batch.push(PhysicalSpiDelivery {
                intid: slot.intid,
                target_mpidr: slot.target_mpidr,
                route_policy: if slot.armed {
                    PhysicalSpiRoutePolicy::Preserve
                } else {
                    PhysicalSpiRoutePolicy::Configure
                },
            });
        }

        if !state.delivery_batch.is_empty() {
            controller.deliver_spis(&state.delivery_batch)?;
        }
        for slot in state.slots.iter_mut().filter(|slot| slot.queued) {
            slot.queued = false;
            slot.armed = true;
        }
        state.owner = PassthroughInterfaceOwner::Guest;
        Ok(())
    }

    /// Reclaims pending SPIs and transfers interface ownership to the host.
    pub(crate) fn complete_guest_exit(
        &self,
        vcpu_id: usize,
        controller: &mut dyn PassthroughSpiController,
    ) -> AxVmResult {
        let mut vcpus = self.vcpus.lock();
        let state = vcpus.get_mut(vcpu_id).ok_or_else(|| {
            AxVmError::invalid_state(
                "complete passthrough guest exit",
                format_args!("vCPU {vcpu_id} has no runtime gate"),
            )
        })?;
        if state.owner != PassthroughInterfaceOwner::Guest {
            return Err(AxVmError::invalid_state(
                "complete passthrough guest exit",
                format_args!("vCPU {vcpu_id} interface is not guest-owned"),
            ));
        }

        state.reclaim_batch.clear();
        for (slot_index, slot) in state.slots.iter().enumerate() {
            if slot.armed {
                debug_assert!(state.reclaim_batch.len() < state.reclaim_batch.capacity());
                state
                    .reclaim_batch
                    .push(PhysicalSpiReclaim::new(slot_index, slot.intid));
            }
        }
        if !state.reclaim_batch.is_empty() {
            controller.reclaim_spis(&mut state.reclaim_batch)?;
        }

        for reclaim in &state.reclaim_batch {
            let observed = reclaim.state.ok_or_else(|| {
                AxVmError::invalid_state(
                    "complete passthrough guest exit",
                    format_args!("INTID {} reclamation returned no state", reclaim.intid),
                )
            })?;
            let slot = &mut state.slots[reclaim.slot_index];
            slot.queued |= observed.pending;
            slot.armed = observed.active;
        }
        state.owner = PassthroughInterfaceOwner::Host;
        Ok(())
    }

    pub(crate) fn has_queued_spi(&self, vcpu_id: usize) -> bool {
        self.vcpus
            .lock()
            .get(vcpu_id)
            .is_some_and(|state| state.slots.iter().any(|slot| slot.queued))
    }
}
