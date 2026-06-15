//! 最简 IPv4 DHCP 服务器(用于 SoftAP 模式给单个客户端分配地址)。
//!
//! 不依赖 smoltcp 的 DHCP socket,与现有 DHCP 客户端一样手工解析/封装
//! `DhcpRepr` → `UdpRepr` → `Ipv4Repr`。处理 Discover→Offer、Request→Ack。
//! 仅支持单客户端、单地址租约,够 AP 验证 ping/ssh 用。

use alloc::{vec, vec::Vec};

use smoltcp::{
    phy::ChecksumCapabilities,
    wire::{
        DHCP_CLIENT_PORT, DHCP_SERVER_PORT, DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress,
        IpAddress, IpProtocol, Ipv4Address, Ipv4Packet, Ipv4Repr, UdpPacket, UdpRepr,
    },
};

/// 租约时长(秒)
const LEASE_SECS: u32 = 86400;

/// DHCP 服务器配置/状态。
pub struct DhcpServer {
    /// 服务器自身 IP(同时作为 gateway / server identifier)
    pub server_ip: Ipv4Address,
    /// 分配给客户端的 IP
    pub client_ip: Ipv4Address,
    /// 子网掩码
    pub subnet_mask: Ipv4Address,
    /// 设备索引(回复从该设备广播出去)
    pub dev: usize,
    /// 已分配给哪个 MAC(简单单客户端记录)
    leased_to: Option<EthernetAddress>,
}

impl DhcpServer {
    pub fn new(
        dev: usize,
        server_ip: Ipv4Address,
        client_ip: Ipv4Address,
        subnet_mask: Ipv4Address,
    ) -> Self {
        Self {
            server_ip,
            client_ip,
            subnet_mask,
            dev,
            leased_to: None,
        }
    }

    /// 解析一个入站以太网负载(IPv4 包)。若是发给本服务器的 DHCP
    /// Discover/Request,返回要广播回去的完整 IPv4 应答包字节。
    pub fn process_packet(&mut self, dev: usize, packet: &[u8]) -> Option<Vec<u8>> {
        if dev != self.dev {
            return None;
        }

        let ipv4_packet = Ipv4Packet::new_checked(packet).ok()?;
        let ipv4_repr = Ipv4Repr::parse(&ipv4_packet, &ChecksumCapabilities::default()).ok()?;
        if ipv4_repr.next_header != IpProtocol::Udp {
            return None;
        }

        let udp_packet = UdpPacket::new_checked(ipv4_packet.payload()).ok()?;
        let udp_repr = UdpRepr::parse(
            &udp_packet,
            &IpAddress::Ipv4(ipv4_repr.src_addr),
            &IpAddress::Ipv4(ipv4_repr.dst_addr),
            &ChecksumCapabilities::default(),
        )
        .ok()?;
        // 客户端 → 服务器:src=68, dst=67
        if udp_repr.src_port != DHCP_CLIENT_PORT || udp_repr.dst_port != DHCP_SERVER_PORT {
            return None;
        }

        let dhcp_packet = DhcpPacket::new_checked(udp_packet.payload()).ok()?;
        let dhcp_repr = DhcpRepr::parse(&dhcp_packet).ok()?;

        let client_mac = dhcp_repr.client_hardware_address;
        let xid = dhcp_repr.transaction_id;

        let reply_type = match dhcp_repr.message_type {
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
    }

    /// 构造 Offer/Ack 应答(完整 IPv4 包,广播)。
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
