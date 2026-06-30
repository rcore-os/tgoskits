//! Ingress packet metadata carried through smoltcp.
//!
//! smoltcp exposes only a small packet metadata id. ax-net uses that id to
//! carry Linux-visible receive-side QoS metadata from the router RX token to
//! datagram sockets without keeping a side table.

use smoltcp::{
    phy::PacketMeta,
    wire::{IpVersion, Ipv4Packet, Ipv6Packet},
};

const RX_QOS_META_MARK: u32 = 0xa7 << 24;
const RX_QOS_META_MARK_MASK: u32 = 0xff << 24;
const RX_QOS_META_VERSION_SHIFT: u32 = 8;

/// Received IP traffic-class metadata that can be reported through recvmsg cmsg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceivedTrafficClass {
    /// IPv4 TOS byte.
    Ipv4(u8),
    /// IPv6 traffic-class byte.
    Ipv6(u8),
}

/// Builds smoltcp packet metadata for one received IP packet.
pub(crate) fn packet_meta_for_rx_packet(packet: &[u8]) -> PacketMeta {
    let mut meta = PacketMeta::default();
    if let Some(traffic_class) = traffic_class_from_packet(packet) {
        meta.id = encode_traffic_class(traffic_class);
    }
    meta
}

/// Decodes receive-side traffic-class metadata from smoltcp packet metadata.
pub(crate) fn received_traffic_class(meta: PacketMeta) -> Option<ReceivedTrafficClass> {
    if meta.id & RX_QOS_META_MARK_MASK != RX_QOS_META_MARK {
        return None;
    }

    let value = (meta.id & 0xff) as u8;
    match (meta.id >> RX_QOS_META_VERSION_SHIFT) & 0xff {
        4 => Some(ReceivedTrafficClass::Ipv4(value)),
        6 => Some(ReceivedTrafficClass::Ipv6(value)),
        _ => None,
    }
}

fn traffic_class_from_packet(packet: &[u8]) -> Option<ReceivedTrafficClass> {
    match IpVersion::of_packet(packet).ok()? {
        IpVersion::Ipv4 => {
            let packet = Ipv4Packet::new_checked(packet).ok()?;
            Some(ReceivedTrafficClass::Ipv4(
                (packet.dscp() << 2) | packet.ecn(),
            ))
        }
        IpVersion::Ipv6 => {
            let packet = Ipv6Packet::new_checked(packet).ok()?;
            Some(ReceivedTrafficClass::Ipv6(packet.traffic_class()))
        }
    }
}

fn encode_traffic_class(traffic_class: ReceivedTrafficClass) -> u32 {
    let (version, value) = match traffic_class {
        ReceivedTrafficClass::Ipv4(value) => (4, value),
        ReceivedTrafficClass::Ipv6(value) => (6, value),
    };
    RX_QOS_META_MARK | (version << RX_QOS_META_VERSION_SHIFT) | u32::from(value)
}

#[cfg(test)]
mod tests {
    use smoltcp::wire::{IpProtocol, Ipv4Address, Ipv4Repr, Ipv6Address, Ipv6Repr};

    use super::*;

    #[test]
    fn ipv4_tos_round_trips_through_packet_meta() {
        let repr = Ipv4Repr {
            src_addr: Ipv4Address::new(127, 0, 0, 1),
            dst_addr: Ipv4Address::new(127, 0, 0, 1),
            next_header: IpProtocol::Udp,
            payload_len: 0,
            hop_limit: 64,
        };
        let mut packet = [0; 20];
        {
            let mut packet = Ipv4Packet::new_unchecked(&mut packet[..]);
            repr.emit(&mut packet, &Default::default());
            packet.set_dscp(0x0b);
            packet.set_ecn(0x02);
        }

        assert_eq!(
            received_traffic_class(packet_meta_for_rx_packet(&packet)),
            Some(ReceivedTrafficClass::Ipv4(0x2e))
        );
    }

    #[test]
    fn ipv6_traffic_class_round_trips_through_packet_meta() {
        let repr = Ipv6Repr {
            src_addr: Ipv6Address::LOCALHOST,
            dst_addr: Ipv6Address::LOCALHOST,
            next_header: IpProtocol::Udp,
            payload_len: 0,
            hop_limit: 64,
        };
        let mut packet = [0; 40];
        {
            let mut packet = Ipv6Packet::new_unchecked(&mut packet[..]);
            repr.emit(&mut packet);
            packet.set_traffic_class(0x2e);
        }

        assert_eq!(
            received_traffic_class(packet_meta_for_rx_packet(&packet)),
            Some(ReceivedTrafficClass::Ipv6(0x2e))
        );
    }
}
