//! Private startup layout for the board browser console.

use alloc::{format, string::String, vec::Vec};

pub(super) const MAX_GUEST_CONSOLES: usize = 3;

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Endpoint {
    pub(super) lane: ConsoleLane,
    pub(super) vm_id: Option<usize>,
    pub(super) route: String,
    pub(super) display_name: String,
}

/// Plans one immutable browser layout from the VMs configured at startup.
pub(super) fn plan_endpoints(mut guests: Vec<(usize, String)>) -> Vec<Endpoint> {
    guests.sort_by_key(|(vm_id, _)| *vm_id);

    let mut endpoints = Vec::with_capacity(guests.len().min(MAX_GUEST_CONSOLES) + 1);
    endpoints.push(Endpoint {
        lane: ConsoleLane::MANAGEMENT,
        vm_id: None,
        route: "axvisor".into(),
        display_name: "Axvisor".into(),
    });
    for (slot, (vm_id, configured_name)) in guests.into_iter().take(MAX_GUEST_CONSOLES).enumerate()
    {
        endpoints.push(Endpoint {
            lane: ConsoleLane::guest(slot),
            vm_id: Some(vm_id),
            route: format!("vm-{vm_id}"),
            display_name: if configured_name.is_empty() {
                format!("VM {vm_id}")
            } else {
                configured_name
            },
        });
    }
    endpoints
}
