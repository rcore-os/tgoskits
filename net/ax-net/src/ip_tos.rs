//! Per-socket egress IP_TOS handling.
//!
//! smoltcp exposes hop-limit setters on TCP/UDP sockets, but it does not expose
//! an IP_TOS/traffic-class setter. ax-net therefore records socket-owned egress
//! TOS policy and applies it to smoltcp-emitted IP packets at the router
//! boundary, after the IP header has been generated and before the packet is
//! handed to loopback or a concrete device.

use ax_kspin::PreemptLazy as LazyLock;
use ax_sync::SpinMutex;
use hashbrown::HashMap;
use smoltcp::wire::{
    IpAddress, IpEndpoint, IpListenEndpoint, IpProtocol, IpVersion, Ipv4Packet, Ipv6Packet,
    TcpPacket, UdpPacket,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct EgressIpTosKey {
    protocol: u8,
    local_addr: Option<IpAddress>,
    local_port: u16,
    remote_addr: Option<IpAddress>,
    remote_port: Option<u16>,
}

impl EgressIpTosKey {
    pub(crate) fn exact(
        protocol: IpProtocol,
        local: IpEndpoint,
        remote: IpEndpoint,
    ) -> Option<Self> {
        if local.port == 0 || remote.port == 0 {
            return None;
        }
        Some(Self {
            protocol: protocol.into(),
            local_addr: Some(local.addr),
            local_port: local.port,
            remote_addr: Some(remote.addr),
            remote_port: Some(remote.port),
        })
    }

    pub(crate) fn listener(protocol: IpProtocol, local: IpListenEndpoint) -> Option<Self> {
        if local.port == 0 {
            return None;
        }
        Some(Self {
            protocol: protocol.into(),
            local_addr: local.addr,
            local_port: local.port,
            remote_addr: None,
            remote_port: None,
        })
    }
}

static EGRESS_IP_TOS: LazyLock<SpinMutex<HashMap<EgressIpTosKey, u8>>> =
    LazyLock::new(|| SpinMutex::new(HashMap::new()));

pub(crate) fn set_egress_ip_tos(key: EgressIpTosKey, tos: u8) {
    let mut table = EGRESS_IP_TOS.lock();
    if tos == 0 {
        table.remove(&key);
    } else {
        table.insert(key, tos);
    }
}

pub(crate) fn clear_egress_ip_tos(key: EgressIpTosKey) {
    EGRESS_IP_TOS.lock().remove(&key);
}

pub(crate) fn apply_egress_ip_tos(packet: &mut [u8]) {
    let tos = packet_egress_ip_tos(packet);
    if tos != 0 {
        apply_ip_tos(packet, tos);
    }
}

pub(crate) fn apply_ip_tos(packet: &mut [u8], tos: u8) {
    match IpVersion::of_packet(packet) {
        Ok(IpVersion::Ipv4) => {
            let Ok(mut packet) = Ipv4Packet::new_checked(packet) else {
                return;
            };
            packet.set_dscp(tos >> 2);
            packet.set_ecn(tos & 0x03);
            packet.fill_checksum();
        }
        Ok(IpVersion::Ipv6) => {
            let Ok(mut packet) = Ipv6Packet::new_checked(packet) else {
                return;
            };
            packet.set_traffic_class(tos);
        }
        Err(_) => {}
    }
}

fn packet_egress_ip_tos(packet: &[u8]) -> u8 {
    let (protocol, local_addr, remote_addr, payload) = match IpVersion::of_packet(packet) {
        Ok(IpVersion::Ipv4) => {
            let Ok(packet) = Ipv4Packet::new_checked(packet) else {
                return 0;
            };
            (
                packet.next_header(),
                IpAddress::Ipv4(packet.src_addr()),
                IpAddress::Ipv4(packet.dst_addr()),
                packet.payload(),
            )
        }
        Ok(IpVersion::Ipv6) => {
            let Ok(packet) = Ipv6Packet::new_checked(packet) else {
                return 0;
            };
            (
                packet.next_header(),
                IpAddress::Ipv6(packet.src_addr()),
                IpAddress::Ipv6(packet.dst_addr()),
                packet.payload(),
            )
        }
        Err(_) => return 0,
    };

    let (local_port, remote_port) = match protocol {
        IpProtocol::Tcp => {
            let Ok(packet) = TcpPacket::new_checked(payload) else {
                return 0;
            };
            (packet.src_port(), packet.dst_port())
        }
        IpProtocol::Udp => {
            let Ok(packet) = UdpPacket::new_checked(payload) else {
                return 0;
            };
            (packet.src_port(), packet.dst_port())
        }
        _ => return 0,
    };

    egress_ip_tos(
        protocol,
        IpEndpoint {
            addr: local_addr,
            port: local_port,
        },
        IpEndpoint {
            addr: remote_addr,
            port: remote_port,
        },
    )
}

fn egress_ip_tos(protocol: IpProtocol, local: IpEndpoint, remote: IpEndpoint) -> u8 {
    if local.port == 0 || remote.port == 0 {
        return 0;
    }

    let table = EGRESS_IP_TOS.lock();
    let protocol = protocol.into();
    let candidates = [
        EgressIpTosKey {
            protocol,
            local_addr: Some(local.addr),
            local_port: local.port,
            remote_addr: Some(remote.addr),
            remote_port: Some(remote.port),
        },
        EgressIpTosKey {
            protocol,
            local_addr: None,
            local_port: local.port,
            remote_addr: Some(remote.addr),
            remote_port: Some(remote.port),
        },
        EgressIpTosKey {
            protocol,
            local_addr: Some(local.addr),
            local_port: local.port,
            remote_addr: None,
            remote_port: None,
        },
        EgressIpTosKey {
            protocol,
            local_addr: None,
            local_port: local.port,
            remote_addr: None,
            remote_port: None,
        },
    ];

    candidates
        .iter()
        .find_map(|key| table.get(key).copied())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use smoltcp::wire::{Ipv4Address, Ipv6Address};

    use super::*;

    #[test]
    fn exact_policy_takes_precedence_over_listener_policy() {
        let local = IpEndpoint {
            addr: IpAddress::Ipv4(Ipv4Address::new(192, 0, 2, 10)),
            port: 41001,
        };
        let remote = IpEndpoint {
            addr: IpAddress::Ipv4(Ipv4Address::new(198, 51, 100, 20)),
            port: 22,
        };
        let listener = EgressIpTosKey::listener(
            IpProtocol::Tcp,
            IpListenEndpoint {
                addr: None,
                port: local.port,
            },
        )
        .unwrap();
        let exact = EgressIpTosKey::exact(IpProtocol::Tcp, local, remote).unwrap();

        set_egress_ip_tos(listener, 0x10);
        set_egress_ip_tos(exact, 0x48);
        assert_eq!(egress_ip_tos(IpProtocol::Tcp, local, remote), 0x48);

        clear_egress_ip_tos(exact);
        assert_eq!(egress_ip_tos(IpProtocol::Tcp, local, remote), 0x10);
        clear_egress_ip_tos(listener);
    }

    #[test]
    fn apply_ip_tos_updates_ipv4_header_and_checksum() {
        let mut packet = [0u8; 20];
        {
            let mut packet = Ipv4Packet::new_unchecked(&mut packet[..]);
            packet.set_version(4);
            packet.set_header_len(20);
            packet.set_total_len(20);
            packet.set_hop_limit(64);
            packet.set_next_header(IpProtocol::Tcp);
            packet.set_src_addr(Ipv4Address::new(192, 0, 2, 10));
            packet.set_dst_addr(Ipv4Address::new(198, 51, 100, 20));
            packet.fill_checksum();
        }

        apply_ip_tos(&mut packet, 0x2e);

        let packet = Ipv4Packet::new_checked(&packet[..]).unwrap();
        assert_eq!(packet.dscp(), 0x0b);
        assert_eq!(packet.ecn(), 0x02);
        assert!(packet.verify_checksum());
    }

    #[test]
    fn apply_ip_tos_updates_ipv6_traffic_class() {
        let mut packet = [0u8; 40];
        {
            let mut packet = Ipv6Packet::new_unchecked(&mut packet[..]);
            packet.set_version(6);
            packet.set_payload_len(0);
            packet.set_hop_limit(64);
            packet.set_next_header(IpProtocol::Tcp);
            packet.set_src_addr(Ipv6Address::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
            packet.set_dst_addr(Ipv6Address::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 2));
        }

        apply_ip_tos(&mut packet, 0xb8);

        let packet = Ipv6Packet::new_checked(&packet[..]).unwrap();
        assert_eq!(packet.traffic_class(), 0xb8);
    }
}
