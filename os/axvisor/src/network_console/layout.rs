//! Runtime console layout for the board browser console.
//!
//! The layout is a fixed-size lane table: lane 0 is the Axvisor management
//! console and lanes 1..=MAX_GUEST_CONSOLES belong to guests. A guest lane is
//! allocated when its VM is created (smallest free slot, so a released lane is
//! reusable) and freed when the VM is removed. That is why the table is a runtime
//! registry rather than a startup snapshot: the browser must see the same guest
//! set the VM registry has, however late a VM appears.

use alloc::{format, string::String, vec::Vec};

/// Number of guest consoles the fixed lane table can hold.
pub(super) const MAX_GUEST_CONSOLES: usize = 8;

/// Route of the Axvisor management console, which always occupies lane 0.
pub(super) const MANAGEMENT_ROUTE: &str = "axvisor";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ConsoleLane(usize);

impl ConsoleLane {
    pub(super) const COUNT: usize = MAX_GUEST_CONSOLES + 1;
    pub(super) const MANAGEMENT: Self = Self(0);

    const fn guest(slot: usize) -> Self {
        assert!(slot < MAX_GUEST_CONSOLES);
        Self(slot + 1)
    }

    pub(super) const fn index(self) -> usize {
        self.0
    }
}

/// One browser-visible console: its lane, its owning VM, and its route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Endpoint {
    pub(super) lane: ConsoleLane,
    pub(super) vm_id: Option<usize>,
    pub(super) route: String,
    pub(super) display_name: String,
}

/// Outcome of allocating a guest lane.
///
/// The caller needs to know whether this call took a lane, because only a lane
/// it took itself may be given back. A VM that already owns a lane keeps it
/// ([`LaneAllocation::Reused`]), so a failed creation that collides with a live
/// VM must not release the lane that VM is using.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LaneAllocation {
    /// The VM already owned a lane, which is left untouched.
    Reused,
    /// This call took a free lane for the VM.
    Allocated,
}

/// Every guest lane is taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LayoutFull;

/// The lane table: the management endpoint plus one optional guest per slot.
pub(super) struct Layout {
    guests: [Option<Endpoint>; MAX_GUEST_CONSOLES],
}

impl Layout {
    pub(super) const fn new() -> Self {
        Self {
            guests: [const { None }; MAX_GUEST_CONSOLES],
        }
    }

    /// Every visible endpoint: the management console first, then guests in lane
    /// order.
    ///
    /// A freed lane disappears from the listing instead of leaving a hole, and a
    /// guest can appear at any lane, so clients must order by themselves if they
    /// care.
    pub(super) fn endpoints(&self) -> Vec<Endpoint> {
        let mut endpoints = Vec::with_capacity(MAX_GUEST_CONSOLES + 1);
        endpoints.push(management_endpoint());
        endpoints.extend(self.guests.iter().flatten().cloned());
        endpoints
    }

    /// The guest endpoint owned by `vm_id`, if that VM has a lane.
    pub(super) fn guest(&self, vm_id: usize) -> Option<&Endpoint> {
        self.guests
            .iter()
            .flatten()
            .find(|endpoint| endpoint.vm_id == Some(vm_id))
    }

    /// The endpoint served on `route`, including the management console.
    pub(super) fn by_route(&self, route: &str) -> Option<Endpoint> {
        if route == MANAGEMENT_ROUTE {
            return Some(management_endpoint());
        }
        self.guests
            .iter()
            .flatten()
            .find(|endpoint| endpoint.route == route)
            .cloned()
    }

    /// The endpoint on `lane`, including the management console.
    pub(super) fn by_lane(&self, lane: ConsoleLane) -> Option<Endpoint> {
        if lane == ConsoleLane::MANAGEMENT {
            return Some(management_endpoint());
        }
        self.guests
            .iter()
            .flatten()
            .find(|endpoint| endpoint.lane == lane)
            .cloned()
    }

    /// Allocates the smallest free guest lane for `vm_id`.
    ///
    /// A VM that already owns a lane keeps it, so retrying a registration is
    /// idempotent instead of moving the guest to another lane.
    pub(super) fn allocate(
        &mut self,
        vm_id: usize,
        name: &str,
    ) -> Result<LaneAllocation, LayoutFull> {
        if self.guest(vm_id).is_some() {
            return Ok(LaneAllocation::Reused);
        }
        let Some(slot) = self.guests.iter().position(Option::is_none) else {
            return Err(LayoutFull);
        };
        self.guests[slot] = Some(Endpoint {
            lane: ConsoleLane::guest(slot),
            vm_id: Some(vm_id),
            route: format!("vm-{vm_id}"),
            display_name: if name.is_empty() {
                format!("VM {vm_id}")
            } else {
                name.into()
            },
        });
        Ok(LaneAllocation::Allocated)
    }

    /// Frees the lane owned by `vm_id` and returns the endpoint it held.
    pub(super) fn release(&mut self, vm_id: usize) -> Option<Endpoint> {
        let slot = self.guests.iter().position(|guest| {
            guest
                .as_ref()
                .is_some_and(|endpoint| endpoint.vm_id == Some(vm_id))
        })?;
        self.guests[slot].take()
    }
}

fn management_endpoint() -> Endpoint {
    Endpoint {
        lane: ConsoleLane::MANAGEMENT,
        vm_id: None,
        route: MANAGEMENT_ROUTE.into(),
        display_name: "Axvisor".into(),
    }
}
