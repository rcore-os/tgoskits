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

use ax_std::os::arceos::modules::ax_net::{self, InterfaceKind, uplink as net_uplink};

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

    let Some((physical, physical_mac)) = physical_interface() else {
        warn!("AxVisor physical uplink skipped: no wired Ethernet host interface is present");
        return;
    };

    let runtime = net_uplink::install(net_uplink::DEFAULT_EGRESS_DEPTH);
    runtime.bind_device(&physical);
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
        physical,
        physical_mac[0],
        physical_mac[1],
        physical_mac[2],
        physical_mac[3],
        physical_mac[4],
        physical_mac[5],
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

/// Selects the wired Ethernet interface whose MAC filter the driver flipped.
///
/// The driver-side `net-l2-uplink` request only opens the RTL8125 filter, so
/// the bridge has to bind that same port. Orange Pi 5 Plus has exactly one
/// wired NIC, and AxVisor names wired devices `eth*` while Wi-Fi keeps its
/// driver name; the selection prefers `eth*` and otherwise falls back to the
/// first Ethernet interface with a MAC, warning when several candidates exist
/// so a board log makes the binding explicit.
fn physical_interface() -> Option<(String, [u8; 6])> {
    let candidates: Vec<(String, [u8; 6])> = ax_net::interfaces()
        .into_iter()
        .filter(|interface| interface.kind == InterfaceKind::Ethernet)
        .filter_map(|interface| interface.mac.map(|mac| (interface.name, mac.0)))
        .collect();
    match candidates.len() {
        0 => None,
        1 => candidates.into_iter().next(),
        _ => {
            warn!(
                "AxVisor physical uplink sees {} Ethernet interfaces; binding the first wired \
                 (eth*) interface so the RTL8125 filter and the bridge target match",
                candidates.len()
            );
            let wired = candidates
                .iter()
                .find(|(name, _)| name.starts_with("eth"))
                .cloned();
            wired.or_else(|| candidates.into_iter().next())
        }
    }
}

fn host_mac_count() -> usize {
    ax_net::interfaces()
        .into_iter()
        .filter(|interface| interface.mac.is_some())
        .count()
}
