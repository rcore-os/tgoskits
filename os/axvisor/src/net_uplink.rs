//! Physical NIC layer-2 uplink wiring for AxVisor.
//!
//! AxVisor keeps one capability-selected physical network interface under the
//! ArceOS host network stack so the host DHCP address and console keep working.
//! Guest virtio-net devices on the internal `VirtualSwitch` are bridged to that
//! selected interface through the bounded `ax-net` uplink instead of taking the device over,
//! which would break the host stack and cannot clone the driver.
//!
//! * Egress: [`UplinkEgress`] submits frames that must leave the fabric into the
//!   bounded, non-blocking `ax-net` ring, which `ax-net` drains on the protocol
//!   executor so the frame still reaches hardware only through the NIC owner.
//! * Ingress: `ax-net` copies every physical frame to [`SwitchIngress`] before
//!   its host-MAC filter, so frames for guest MACs reach the switch too.

use std::sync::Arc;

use ax_std::os::arceos::modules::ax_net::{
    self, InterfaceInfo, InterfaceKind, NetRxMode, uplink as net_uplink,
};

/// Physical ingress adapter feeding host-received frames into the switch.
struct SwitchIngress;

impl net_uplink::IngressSink for SwitchIngress {
    fn deliver_physical_rx(&self, frame: &[u8]) {
        axvm::switch_from_physical_rx(frame);
    }
}

/// Physical egress adapter submitting guest frames into the uplink ring.
struct UplinkEgress {
    runtime: Arc<net_uplink::UplinkRuntime>,
}

impl axvm::PhysicalUplink for UplinkEgress {
    fn submit_guest_egress(&self, frame: &[u8]) -> Result<(), axvm::PhysicalUplinkError> {
        self.runtime
            .submit_egress(frame)
            .map_err(|error| match error {
                net_uplink::UplinkEgressError::Full => axvm::PhysicalUplinkError::Backpressure,
                net_uplink::UplinkEgressError::InvalidFrame => {
                    axvm::PhysicalUplinkError::InvalidFrame
                }
            })
    }
}

/// Installs the physical L2 uplink before any guest device is created.
///
/// Reserving the host MACs must precede device-node creation so a guest cannot
/// claim the host address; the switch itself is created lazily by the guest
/// devices, so installing the ingress sink earlier is safe.
pub(crate) fn start() {
    if axvm::physical_uplink_installed() {
        warn!("AxVisor physical uplink is already installed; skipping duplicate setup");
        return;
    }
    reserve_host_macs();

    // Selecting the interface and enabling its filter are one step: a port
    // whose driver has no address-filter control cannot carry guest MACs, so
    // the search moves to the next candidate. The uplink is bound to the
    // interface whose own filter request succeeded, so no unrelated port is
    // left with the filter enabled.
    let Some(physical) = physical_interface() else {
        warn!(
            "AxVisor physical uplink skipped: no Ethernet host interface accepts guest \
             MACs (no RX address-filter control)"
        );
        return;
    };
    let Some(physical_mac) = physical.mac else {
        warn!("AxVisor physical uplink skipped: selected interface has no MAC address");
        return;
    };

    let runtime = Arc::new(net_uplink::UplinkRuntime::new(
        net_uplink::DEFAULT_EGRESS_DEPTH,
    ));
    runtime.bind_interface(physical.id);
    runtime.set_ingress_sink(Arc::new(SwitchIngress));
    let egress = Arc::new(UplinkEgress {
        runtime: runtime.clone(),
    });
    if !axvm::install_physical_uplink(egress) {
        // Do not leave a candidate NIC in all-unicast mode when another
        // initializer won the fabric registration race.
        let _ = ax_net::set_interface_rx_mode(physical.id, NetRxMode::normal());
        warn!("AxVisor physical uplink egress adapter was already installed");
        return;
    }
    if !net_uplink::install(runtime.clone()) {
        // A successful AxVM registration without a matching ax-net runtime is
        // an initialization invariant violation. Keep the candidate NIC in
        // host mode and leave guest creation to the caller's fail-closed path.
        let _ = ax_net::set_interface_rx_mode(physical.id, NetRxMode::normal());
        warn!("AxVisor physical uplink runtime was already published");
        return;
    }

    info!(
        "AxVisor physical uplink ready: interface={}, \
         mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, egress_depth={}, host_macs={}",
        physical.name,
        physical_mac.0[0],
        physical_mac.0[1],
        physical_mac.0[2],
        physical_mac.0[3],
        physical_mac.0[4],
        physical_mac.0[5],
        net_uplink::DEFAULT_EGRESS_DEPTH,
        host_mac_count(),
    );
}

/// Reserves every host interface MAC so no guest port can claim it.
fn reserve_host_macs() {
    for interface in ax_net::interfaces() {
        if let Some(mac) = interface.mac {
            axvm::reserve_host_mac(mac.0);
        }
    }
}

/// Selects an Ethernet interface whose own control instance can accept
/// every unicast destination needed by the bridge.
///
/// The choice is driven by device capability rather than by interface name or
/// probe order: AxVisor enables the hardware filter through the published
/// interface's control endpoint and then binds the uplink to that exact
/// interface. A candidate is used only when its own request succeeds, so the
/// filter is active on the selected NIC before the uplink becomes reachable
/// and no unrelated port is left with the filter enabled.
fn physical_interface() -> Option<InterfaceInfo> {
    for interface in ax_net::interfaces() {
        if interface.kind != InterfaceKind::Ethernet || interface.mac.is_none() {
            continue;
        }
        match ax_net::set_interface_rx_mode(interface.id, NetRxMode::all_unicast()) {
            Ok(()) => {
                info!(
                    "AxVisor physical uplink enabling guest-MAC RX filter on {}",
                    interface.name
                );
                return Some(interface);
            }
            Err(error) => info!(
                "AxVisor physical uplink skipping {}: RX address-filter control unavailable \
                 ({error})",
                interface.name
            ),
        }
    }
    None
}

fn host_mac_count() -> usize {
    ax_net::interfaces()
        .into_iter()
        .filter(|interface| interface.mac.is_some())
        .count()
}
