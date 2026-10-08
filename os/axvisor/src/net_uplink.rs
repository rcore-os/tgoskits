//! Physical NIC layer-2 uplink wiring for AxVisor.
//!
//! AxVisor keeps exactly one physical NIC (the RTL8125) under the ArceOS host
//! network stack so the host DHCP address and console keep working. Guest
//! virtio-net devices on the internal `VirtualSwitch` are bridged to that single
//! NIC through the bounded `ax-net` uplink instead of taking the device over,
//! which would break the host stack and cannot clone the driver.
//!
//! * Egress: [`UplinkEgress`] submits frames that must leave the fabric into the
//!   bounded, non-blocking `ax-net` ring, which `ax-net` drains on the protocol
//!   executor so the frame still reaches hardware only through the NIC owner.
//! * Ingress: `ax-net` copies every physical frame to [`SwitchIngress`] before
//!   its host-MAC filter, so frames for guest MACs reach the switch too.

use std::sync::Arc;

use ax_std::os::arceos::modules::ax_net::{
    self, InterfaceInfo, InterfaceKind, uplink as net_uplink,
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
    fn submit_guest_egress(&self, frame: &[u8]) -> bool {
        self.runtime.submit_egress(frame).is_ok()
    }
}

/// Installs the physical L2 uplink before any guest device is created.
///
/// Reserving the host MACs must precede device-node creation so a guest cannot
/// claim the host address; the switch itself is created lazily by the guest
/// devices, so installing the ingress sink earlier is safe.
pub(crate) fn start() {
    reserve_host_macs();

    // Selecting the interface and enabling its filter are one step: a port
    // whose driver has no address-filter control cannot carry guest MACs, so
    // the search moves to the next candidate. The uplink is bound to the
    // interface whose own filter request succeeded, so no unrelated port is
    // left with the filter enabled.
    let Some(physical) = physical_interface() else {
        warn!(
            "AxVisor physical uplink skipped: no wired Ethernet host interface accepts guest \
             MACs (no RX address-filter control)"
        );
        return;
    };
    let Some(physical_mac) = physical.mac else {
        warn!("AxVisor physical uplink skipped: selected interface has no MAC address");
        return;
    };

    let runtime = net_uplink::install(net_uplink::DEFAULT_EGRESS_DEPTH);
    runtime.bind_device(&physical.name);
    runtime.set_ingress_sink(Arc::new(SwitchIngress));
    let egress = Arc::new(UplinkEgress {
        runtime: runtime.clone(),
    });
    if !axvm::install_physical_uplink(egress) {
        warn!("AxVisor physical uplink egress adapter was already installed");
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

/// Selects the wired Ethernet interface whose own control instance can accept
/// every physical unicast address.
///
/// The choice is driven by device capability rather than by interface name or
/// probe order: AxVisor enables the hardware filter through the published
/// interface's control endpoint and then binds the uplink to that exact
/// interface. AxVisor publishes Wi-Fi under the same `Ethernet` kind, but a
/// Wi-Fi driver has no address-filter control, so it reports
/// `OperationNotSupported` and the search moves on. A candidate is used only
/// when its own request succeeded, so the filter is already active on the
/// selected NIC before the uplink becomes reachable and no unrelated port is
/// left with the filter enabled.
fn physical_interface() -> Option<InterfaceInfo> {
    for interface in ax_net::interfaces() {
        if interface.kind != InterfaceKind::Ethernet || interface.mac.is_none() {
            continue;
        }
        match ax_net::set_interface_rx_accept_all_phys(interface.id, true) {
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
