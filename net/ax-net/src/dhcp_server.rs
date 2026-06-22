//! Minimal IPv4 DHCP server for SoftAP-style deployments.
//!
//! The server is intentionally small: it supports one interface, one client
//! lease, and the Discover/Offer plus Request/Ack exchange needed to bring up a
//! directly attached peer. It does not create a smoltcp UDP socket; instead it
//! parses and emits `DhcpRepr`, `UdpRepr`, and `Ipv4Repr` directly in the
//! service data path.
//!
//! # Scope
//!
//! This is a control-plane helper, not a general-purpose DHCP daemon. It keeps
//! no lease database, performs no conflict detection, and only replies to DHCP
//! packets arriving on the configured interface. That makes it suitable for
//! embedded AP validation such as ping or ssh, while keeping the normal socket
//! path free of DHCP server-specific state.

use alloc::{vec, vec::Vec};

use smoltcp::{
    phy::ChecksumCapabilities,
    wire::{
        DHCP_CLIENT_PORT, DHCP_SERVER_PORT, DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress,
        IpAddress, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr, UdpPacket, UdpRepr,
    },
};

use crate::config::InterfaceId;

/// Lease duration advertised in Offer/Ack replies, in seconds.
const LEASE_SECS: u32 = 86400;

/// Parsed DHCP-over-IPv4/UDP packet.
pub(crate) struct ParsedDhcp<'a> {
    pub(crate) src_addr: Ipv4Address,
    pub(crate) udp: UdpRepr,
    pub(crate) dhcp: DhcpRepr<'a>,
}

/// Parses an IPv4 UDP DHCP packet, leaving direction checks to the caller.
pub(crate) fn parse_dhcp_packet<R>(
    packet: &[u8],
    f: impl FnOnce(ParsedDhcp<'_>) -> Option<R>,
) -> Option<R> {
    let ipv4_packet = Ipv4Packet::new_checked(packet).ok()?;
    let ipv4_repr = Ipv4Repr::parse(&ipv4_packet, &ChecksumCapabilities::default()).ok()?;
    if ipv4_repr.next_header != IpProtocol::Udp {
        return None;
    }

    let udp_packet = UdpPacket::new_checked(ipv4_packet.payload()).ok()?;
    let udp = UdpRepr::parse(
        &udp_packet,
        &IpAddress::Ipv4(ipv4_repr.src_addr),
        &IpAddress::Ipv4(ipv4_repr.dst_addr),
        &ChecksumCapabilities::default(),
    )
    .ok()?;
    let dhcp_packet = DhcpPacket::new_checked(udp_packet.payload()).ok()?;
    let dhcp = DhcpRepr::parse(&dhcp_packet).ok()?;
    f(ParsedDhcp {
        src_addr: ipv4_repr.src_addr,
        udp,
        dhcp,
    })
}

/// Minimal DHCP server configuration and one-client lease state.
pub struct DhcpServer {
    /// Server address, also advertised as router and server identifier.
    pub server_ip: Ipv4Address,
    /// Single IPv4 address offered to the client.
    pub client_ip: Ipv4Address,
    /// Subnet mask advertised to the client.
    pub subnet_mask: Ipv4Address,
    /// Router device index used when the service broadcasts replies.
    pub dev: usize,
    /// Interface that is allowed to feed requests into this server.
    interface_id: InterfaceId,
    /// MAC address that accepted the single lease, if any.
    leased_to: Option<EthernetAddress>,
}

impl DhcpServer {
    /// Creates a DHCP helper bound to one router device and interface.
    pub fn new(
        dev: usize,
        interface_id: InterfaceId,
        server_ip: Ipv4Address,
        client_ip: Ipv4Address,
        subnet_mask: Ipv4Address,
    ) -> Self {
        Self {
            server_ip,
            client_ip,
            subnet_mask,
            dev,
            interface_id,
            leased_to: None,
        }
    }

    /// Processes one inbound IPv4 packet and returns a broadcast DHCP reply.
    ///
    /// Non-DHCP traffic, unsupported DHCP message types, or packets from other
    /// interfaces are ignored by returning `None`.
    pub fn process_packet(&mut self, interface_id: InterfaceId, packet: &[u8]) -> Option<Vec<u8>> {
        if interface_id != self.interface_id {
            return None;
        }

        parse_dhcp_packet(packet, |parsed| {
            // Client -> server uses UDP src=68, dst=67.
            if parsed.udp.src_port != DHCP_CLIENT_PORT || parsed.udp.dst_port != DHCP_SERVER_PORT {
                return None;
            }

            let client_mac = parsed.dhcp.client_hardware_address;
            let xid = parsed.dhcp.transaction_id;

            let reply_type = match parsed.dhcp.message_type {
                DhcpMessageType::Discover => {
                    info!(
                        "[dhcp-srv] Discover from {client_mac} -> Offer {}",
                        self.client_ip
                    );
                    DhcpMessageType::Offer
                }
                DhcpMessageType::Request => {
                    self.leased_to = Some(client_mac);
                    info!(
                        "[dhcp-srv] Request from {client_mac} -> Ack {}",
                        self.client_ip
                    );
                    DhcpMessageType::Ack
                }
                _ => return None,
            };

            Some(self.build_reply(client_mac, xid, reply_type))
        })
    }

    /// Builds a complete IPv4 packet containing a DHCP Offer/Ack reply.
    fn build_reply(
        &self,
        client_mac: EthernetAddress,
        xid: u32,
        message_type: DhcpMessageType,
    ) -> Vec<u8> {
        let dhcp_repr = DhcpRepr {
            message_type,
            transaction_id: xid,
            secs: 0,
            client_hardware_address: client_mac,
            client_ip: Ipv4Address::UNSPECIFIED,
            your_ip: self.client_ip,
            server_ip: self.server_ip,
            router: Some(self.server_ip),
            subnet_mask: Some(self.subnet_mask),
            relay_agent_ip: Ipv4Address::UNSPECIFIED,
            broadcast: true,
            requested_ip: None,
            client_identifier: None,
            server_identifier: Some(self.server_ip),
            parameter_request_list: None,
            dns_servers: None,
            max_size: None,
            lease_duration: Some(LEASE_SECS),
            renew_duration: None,
            rebind_duration: None,
            additional_options: &[],
        };
        let udp_repr = UdpRepr {
            src_port: DHCP_SERVER_PORT,
            dst_port: DHCP_CLIENT_PORT,
        };
        let ipv4_repr = Ipv4Repr {
            src_addr: self.server_ip,
            dst_addr: Ipv4Address::BROADCAST,
            next_header: IpProtocol::Udp,
            payload_len: udp_repr.header_len() + dhcp_repr.buffer_len(),
            hop_limit: 64,
        };

        let mut buffer = vec![0; ipv4_repr.buffer_len() + ipv4_repr.payload_len];
        let checksum_caps = ChecksumCapabilities::default();
        let mut ipv4_packet = Ipv4Packet::new_unchecked(&mut buffer);
        ipv4_repr.emit(&mut ipv4_packet, &checksum_caps);
        let mut udp_packet = UdpPacket::new_unchecked(ipv4_packet.payload_mut());
        udp_repr.emit(
            &mut udp_packet,
            &IpAddress::Ipv4(ipv4_repr.src_addr),
            &IpAddress::Ipv4(ipv4_repr.dst_addr),
            dhcp_repr.buffer_len(),
            |payload| {
                dhcp_repr
                    .emit(&mut DhcpPacket::new_unchecked(payload))
                    .expect("failed to emit DHCP reply");
            },
            &checksum_caps,
        );
        buffer
    }
}
