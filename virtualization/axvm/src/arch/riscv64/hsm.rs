//! Fixed RISC-V hart topology for SBI HSM and IPI routing.
//!
//! The control owner plans the guest-visible hart id of every vCPU before any
//! guest can run. A vCPU task captures that plan in its entry payload, so an
//! unbound exit handler resolves HSM and IPI targets from a fixed snapshot
//! instead of querying the complete VM.

use std::{boxed::Box, vec::Vec};

/// Guest-visible hart ids paired with their VM-local vCPU ids.
#[derive(Clone, Debug)]
pub(crate) struct HartTopology {
    entries: Box<[(usize, usize)]>,
}

impl HartTopology {
    /// Snapshots the fixed vCPU placement list as `(vcpu_id, guest_hart_id)`.
    pub(crate) fn new(placements: &[(usize, Option<usize>, usize)]) -> Self {
        let entries = placements
            .iter()
            .map(|(vcpu_id, _cpu_set, guest_hart_id)| (*vcpu_id, *guest_hart_id))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self { entries }
    }

    /// Returns every planned vCPU id in placement order.
    pub(crate) fn vcpu_ids(&self) -> Vec<usize> {
        self.entries
            .iter()
            .map(|(vcpu_id, _guest_hart_id)| *vcpu_id)
            .collect()
    }

    /// Resolves one guest hart id to its VM-local vCPU id.
    pub(crate) fn resolve_hart(&self, guest_hart_id: usize) -> Option<usize> {
        self.entries
            .iter()
            .find_map(|(vcpu_id, configured)| (*configured == guest_hart_id).then_some(*vcpu_id))
    }
}
