//! Full TCP capacity delays new HTTP handshakes instead of refusing them.

use smoltcp::{
    phy::ChecksumCapabilities,
    socket::tcp,
    wire::{
        EthernetFrame, EthernetProtocol, IpEndpoint, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr,
        TcpControl, TcpPacket, TcpRepr,
    },
};

use super::HTTP_PORT;

pub(super) struct SynBackpressure {
    address: Ipv4Address,
    connections: [Option<(IpEndpoint, IpEndpoint)>; 2],
}

impl SynBackpressure {
    /// Takes a snapshot immediately before one ingress frame is processed.
    pub(super) fn when_full(address: Ipv4Address, sockets: [&tcp::Socket<'_>; 2]) -> Option<Self> {
        if sockets.iter().any(|socket| socket.is_listening()) {
            return None;
        }
        Some(Self {
            address,
            connections: sockets
                .map(|socket| socket.local_endpoint().zip(socket.remote_endpoint())),
        })
    }

    pub(super) fn defer(&self, frame: &[u8]) -> bool {
        self.new_http_connection(frame)
            .is_some_and(|connection| !self.connections.contains(&Some(connection)))
    }

    fn new_http_connection(&self, frame: &[u8]) -> Option<(IpEndpoint, IpEndpoint)> {
        let ethernet = EthernetFrame::new_checked(frame).ok()?;
        if ethernet.ethertype() != EthernetProtocol::Ipv4 {
            return None;
        }
        let packet = Ipv4Packet::new_checked(ethernet.payload()).ok()?;
        // Fragment assembly and all malformed packets stay with the original
        // stack. Only a complete, valid initial SYN is eligible for deferral.
        if packet.more_frags()
            || packet.frag_offset() != 0
            || packet.dst_addr() != self.address
            || packet.next_header() != IpProtocol::Tcp
        {
            return None;
        }
        let tcp_packet = TcpPacket::new_checked(packet.payload()).ok()?;
        // Established upload/ACK traffic needs no duplicate checksum pass.
        if tcp_packet.dst_port() != HTTP_PORT || !tcp_packet.syn() || tcp_packet.ack() {
            return None;
        }
        let checksums = ChecksumCapabilities::default();
        let ipv4 = Ipv4Repr::parse(&packet, &checksums).ok()?;
        let tcp = TcpRepr::parse(
            &tcp_packet,
            &ipv4.src_addr.into(),
            &ipv4.dst_addr.into(),
            &checksums,
        )
        .ok()?;
        if tcp.control != TcpControl::Syn {
            return None;
        }
        Some((
            IpEndpoint::new(ipv4.dst_addr.into(), tcp.dst_port),
            IpEndpoint::new(ipv4.src_addr.into(), tcp.src_port),
        ))
    }
}
