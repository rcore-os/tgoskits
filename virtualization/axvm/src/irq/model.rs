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
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct VirtualInterruptId(pub u32);

/// An interrupt event pending delivery to a target vCPU.
///
/// Carries the trigger mode (edge/level) so that architecture injection paths
/// and routers can preserve the semantics declared by the device.
///
/// Will be enqueued into [`VcpuIrqDispatcher`](crate::runtime::VcpuIrqDispatcher)
/// and later drained by the target vCPU run loop for injection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingVcpuInterrupt {
    pub id: VirtualInterruptId,
    pub trigger: InterruptTriggerMode,
}

/// GICv3 list-register injection policy (architecture-independent decision).
///
/// A slot is free when its state is `Invalid` (0); a slot occupied by `vector`
/// blocks another edge for the same vector while Pending (1) or Active (2/3).
/// When no slot is free, the backend must defer instead of failing, so the
/// drained edge can be re-queued and retried.
#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn lr_blocked(slots: &[(u64, u64)], vector: usize) -> bool {
    let mut has_free_slot = false;
    for &(vintid, state) in slots {
        if vintid == vector as u64 && (state & 0b11) != 0 {
            return true;
        }
        if state == 0 {
            has_free_slot = true;
        }
    }
    !has_free_slot
}

/// Returns true when a single-slot backend (GICv2 list register 0 in this
/// project) is occupied by any interrupt, so another edge must be deferred.
#[cfg(any(target_arch = "aarch64", test))]
pub(crate) fn lr_slot_occupied(state: u32) -> bool {
    state != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_vector_pending_blocks_injection() {
        let slots = [(48, 0b01), (0, 0), (0, 0), (0, 0)];
        assert!(lr_blocked(&slots, 48));
    }

    #[test]
    fn same_vector_active_blocks_injection() {
        let slots = [(48, 0b10), (0, 0), (0, 0), (0, 0)];
        assert!(lr_blocked(&slots, 48));
    }

    #[test]
    fn free_slot_allows_injection() {
        let slots = [(30, 0b01), (0, 0b10), (49, 0b10), (0, 0)];
        assert!(!lr_blocked(&slots, 48));
    }

    #[test]
    fn all_slots_occupied_by_other_vectors_defers() {
        let slots = [(30, 0b01), (31, 0b10), (49, 0b10), (50, 0b01)];
        assert!(lr_blocked(&slots, 48));
    }

    #[test]
    fn lr_slot_occupied_treats_any_active_state_as_busy() {
        assert!(!lr_slot_occupied(0));
        assert!(lr_slot_occupied(1));
        assert!(lr_slot_occupied(2));
        assert!(lr_slot_occupied(3));
    }
}
