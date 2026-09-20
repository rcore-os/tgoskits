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

#[cfg(any(test, axtest))]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn uses_at_most_three_sorted_guests() {
        let endpoints = plan_endpoints(
            [7, 5, 9, 3]
                .into_iter()
                .map(|vm_id| (vm_id, vm_id.to_string()))
                .collect(),
        );

        assert_eq!(endpoints.len(), MAX_GUEST_CONSOLES + 1);
        assert_eq!(endpoints[0].route, "axvisor");
        assert_eq!(endpoints[1].vm_id, Some(3));
        assert_eq!(endpoints[2].vm_id, Some(5));
        assert_eq!(endpoints[3].vm_id, Some(7));
        assert_eq!(endpoints[3].lane.index(), 3);
    }

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn uses_configured_names_with_vm_fallback() {
        let endpoints = plan_endpoints(vec![(2, "zephyr".into()), (1, String::new())]);

        assert_eq!(endpoints[1].display_name, "VM 1");
        assert_eq!(endpoints[2].display_name, "zephyr");
        assert_eq!(endpoints[2].route, "vm-2");
    }
}
