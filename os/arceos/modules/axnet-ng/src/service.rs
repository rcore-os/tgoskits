use alloc::{boxed::Box, vec, vec::Vec};
use core::{
    pin::Pin,
    task::{Context, Waker},
};

use ax_hal::time::{NANOS_PER_MICROS, TimeValue, wall_time_nanos};
use ax_task::future::sleep_until;
use smoltcp::{
    iface::{Interface, SocketSet},
    phy::ChecksumCapabilities,
    time::{Duration as SmolDuration, Instant},
    wire::{
        DHCP_CLIENT_PORT, DHCP_SERVER_PORT, DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress,
        HardwareAddress, IpAddress, IpCidr, IpListenEndpoint, IpProtocol, Ipv4Address, Ipv4Cidr,
        Ipv4Packet, Ipv4Repr, UdpPacket, UdpRepr,
    },
};

use crate::{SOCKET_SET, consts::STANDARD_MTU, router::Router};

fn now() -> Instant {
    Instant::from_micros_const((wall_time_nanos() / NANOS_PER_MICROS) as i64)
}

pub struct Service {
    pub iface: Interface,
    router: Router,
    timeout: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    dhcp: Option<DhcpState>,
}

struct DhcpState {
    dev: usize,
    mac: EthernetAddress,
    transaction_id: u32,
    phase: DhcpPhase,
    retry_at: Instant,
    retry: usize,
    offered_address: Option<Ipv4Address>,
    server_identifier: Option<Ipv4Address>,
    address: Option<Ipv4Cidr>,
    dns_servers: Vec<Ipv4Address>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DhcpPhase {
    Discovering,
    Requesting,
    Bound,
}

const DHCP_PARAMETER_REQUEST_LIST: &[u8] = &[1, 3, 6, 42];
const DHCP_MAX_RETRY_SHIFT: usize = 4;

impl DhcpState {
    fn new(dev: usize, mac: EthernetAddress) -> Self {
        Self {
            dev,
            mac,
            transaction_id: dhcp_transaction_id(mac),
            phase: DhcpPhase::Discovering,
            retry_at: Instant::from_micros_const(0),
            retry: 0,
            offered_address: None,
            server_identifier: None,
            address: None,
            dns_servers: Vec::new(),
        }
    }

    fn process_packet(
        &mut self,
        dev: usize,
        packet: &[u8],
        timestamp: Instant,
    ) -> Option<DhcpEvent> {
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
        if udp_repr.src_port != DHCP_SERVER_PORT || udp_repr.dst_port != DHCP_CLIENT_PORT {
            return None;
        }

        let dhcp_packet = DhcpPacket::new_checked(udp_packet.payload()).ok()?;
        let dhcp_repr = DhcpRepr::parse(&dhcp_packet).ok()?;
        if dhcp_repr.client_hardware_address != self.mac
            || dhcp_repr.transaction_id != self.transaction_id
        {
            return None;
        }

        match (self.phase, dhcp_repr.message_type) {
            (DhcpPhase::Discovering, DhcpMessageType::Offer) => {
                if !is_unicast_ipv4(dhcp_repr.your_ip) {
                    return None;
                }
                self.offered_address = Some(dhcp_repr.your_ip);
                self.server_identifier = dhcp_repr.server_identifier.or(Some(ipv4_repr.src_addr));
                self.phase = DhcpPhase::Requesting;
                self.retry = 0;
                self.retry_at = timestamp;
                info!(
                    "eth0: DHCP offered address {} from {}",
                    dhcp_repr.your_ip,
                    self.server_identifier.unwrap_or(ipv4_repr.src_addr)
                );
                None
            }
            (DhcpPhase::Requesting, DhcpMessageType::Ack)
            | (DhcpPhase::Bound, DhcpMessageType::Ack) => {
                let subnet_mask = dhcp_repr.subnet_mask?;
                let prefix_len = IpAddress::Ipv4(subnet_mask).prefix_len()?;
                if !is_unicast_ipv4(dhcp_repr.your_ip) {
                    return None;
                }
                self.phase = DhcpPhase::Bound;
                self.retry = 0;
                let address = Ipv4Cidr::new(dhcp_repr.your_ip, prefix_len);
                Some(DhcpEvent::Configured {
                    address,
                    router: dhcp_repr.router,
                    dns_servers: dhcp_repr
                        .dns_servers
                        .as_ref()
                        .map(|servers| servers.iter().copied().collect())
                        .unwrap_or_default(),
                })
            }
            (_, DhcpMessageType::Nak) => {
                let was_configured = self.address.is_some();
                self.reset(timestamp);
                was_configured.then_some(DhcpEvent::Deconfigured)
            }
            _ => None,
        }
    }

    fn poll_packet(&mut self, timestamp: Instant) -> Option<(usize, IpAddress, Vec<u8>)> {
        if self.phase == DhcpPhase::Bound || timestamp < self.retry_at {
            return None;
        }

        let (message_type, requested_ip, server_identifier) = match self.phase {
            DhcpPhase::Discovering => (DhcpMessageType::Discover, None, None),
            DhcpPhase::Requesting => (
                DhcpMessageType::Request,
                self.offered_address,
                self.server_identifier,
            ),
            DhcpPhase::Bound => return None,
        };

        let retry_delay_secs = 1usize << self.retry.min(DHCP_MAX_RETRY_SHIFT);
        self.retry = self.retry.saturating_add(1);
        self.retry_at = timestamp + SmolDuration::from_secs(retry_delay_secs as u64);

        Some((
            self.dev,
            IpAddress::Ipv4(Ipv4Address::BROADCAST),
            build_dhcp_packet(
                self.mac,
                self.transaction_id,
                message_type,
                requested_ip,
                server_identifier,
            ),
        ))
    }

    fn reset(&mut self, timestamp: Instant) {
        self.transaction_id = dhcp_transaction_id(self.mac);
        self.phase = DhcpPhase::Discovering;
        self.retry_at = timestamp;
        self.retry = 0;
        self.offered_address = None;
        self.server_identifier = None;
        self.address = None;
        self.dns_servers.clear();
    }
}
impl Service {
    pub fn new(mut router: Router) -> Self {
        let config = smoltcp::iface::Config::new(HardwareAddress::Ip);
        let iface = Interface::new(config, &mut router, now());

        Self {
            iface,
            router,
            timeout: None,
            dhcp: None,
        }
    }

    pub fn enable_dhcp(&mut self, dev: usize, mac: EthernetAddress) {
        self.dhcp = Some(DhcpState::new(dev, mac));
        info!("eth0: DHCP enabled");
    }

    pub fn dhcp_enabled(&self) -> bool {
        self.dhcp.is_some()
    }

    pub fn dhcp_configured(&self) -> bool {
        self.dhcp
            .as_ref()
            .is_some_and(|state| state.address.is_some())
    }

    pub fn poll(&mut self, sockets: &mut SocketSet) -> bool {
        let timestamp = now();
        let mut dhcp_events = Vec::new();

        {
            let dhcp = &mut self.dhcp;
            self.router.poll(timestamp, sockets, |dev, packet| {
                if let Some(event) = dhcp
                    .as_mut()
                    .and_then(|state| state.process_packet(dev, packet, timestamp))
                {
                    dhcp_events.push(event);
                }
            });
        }
        for event in dhcp_events {
            self.handle_dhcp_event(event);
        }
        self.iface.poll(timestamp, &mut self.router, sockets);
        let dhcp_poll_next = self.poll_dhcp(timestamp);
        self.router.dispatch(timestamp) || dhcp_poll_next
    }

    fn poll_dhcp(&mut self, timestamp: Instant) -> bool {
        let Some((dev, next_hop, packet)) = self
            .dhcp
            .as_mut()
            .and_then(|state| state.poll_packet(timestamp))
        else {
            return false;
        };

        self.router
            .send_on_device(dev, next_hop, &packet, timestamp)
    }

    fn handle_dhcp_event(&mut self, event: DhcpEvent) {
        match event {
            DhcpEvent::Configured {
                address,
                router,
                dns_servers,
            } => {
                let Some(state) = &mut self.dhcp else {
                    return;
                };
                info!("eth0: DHCP acquired address {address}");
                match router {
                    Some(router) => info!("eth0: DHCP router {router}"),
                    None => info!("eth0: DHCP router not provided"),
                }
                for dns in &dns_servers {
                    info!("eth0: DHCP DNS {dns}");
                }

                Self::set_interface_ipv4(&mut self.iface, state.address, Some(address));
                state.address = Some(address);
                state.dns_servers = dns_servers;
                self.router
                    .set_ipv4_config(state.dev, Some(address), router.map(IpAddress::Ipv4));
            }
            DhcpEvent::Deconfigured => {
                let Some(state) = &mut self.dhcp else {
                    return;
                };
                if state.address.is_some() {
                    info!("eth0: DHCP deconfigured");
                }
                Self::set_interface_ipv4(&mut self.iface, state.address, None);
                state.address = None;
                state.dns_servers.clear();
                self.router.set_ipv4_config(state.dev, None, None);
            }
        }
    }

    fn set_interface_ipv4(
        iface: &mut Interface,
        old_address: Option<Ipv4Cidr>,
        new_address: Option<Ipv4Cidr>,
    ) {
        iface.update_ip_addrs(|ip_addrs| {
            if let Some(old_address) = old_address {
                ip_addrs.retain(|addr| *addr != IpCidr::Ipv4(old_address));
            }
            if let Some(new_address) = new_address {
                let new_address = IpCidr::Ipv4(new_address);
                if !ip_addrs.contains(&new_address) {
                    ip_addrs.push(new_address).unwrap();
                }
            }
        });
    }

    pub fn get_source_address(&self, dst_addr: &IpAddress) -> IpAddress {
        let Some(rule) = self.router.table.lookup(dst_addr) else {
            panic!("no route to destination: {dst_addr}");
        };
        rule.src
    }

    pub fn device_mask_for(&self, endpoint: &IpListenEndpoint) -> u32 {
        match endpoint.addr {
            Some(addr) => self
                .router
                .table
                .lookup(&addr)
                .map_or(0, |it| 1u32 << it.dev),
            None => u32::MAX,
        }
    }

    pub fn register_waker(&mut self, mask: u32, waker: &Waker) {
        let next = self.iface.poll_at(now(), &SOCKET_SET.inner.lock());

        if let Some(t) = next {
            let next = TimeValue::from_micros(t.total_micros() as _);

            // drop old timeout future
            self.timeout = None;

            let mut fut = Box::pin(sleep_until(next));
            let mut cx = Context::from_waker(waker);

            if fut.as_mut().poll(&mut cx).is_ready() {
                waker.wake_by_ref();
                return;
            } else {
                self.timeout = Some(fut);
            }
        }

        for (i, device) in self.router.devices.iter().enumerate() {
            if mask & (1 << i) != 0 {
                device.register_waker(waker);
            }
        }
    }
}

enum DhcpEvent {
    Configured {
        address: Ipv4Cidr,
        router: Option<Ipv4Address>,
        dns_servers: Vec<Ipv4Address>,
    },
    Deconfigured,
}

fn dhcp_transaction_id(mac: EthernetAddress) -> u32 {
    let mut value = (wall_time_nanos() as u32).rotate_left(7);
    for byte in mac.0 {
        value = value.rotate_left(5) ^ u32::from(byte);
    }
    value
}

fn is_unicast_ipv4(addr: Ipv4Address) -> bool {
    addr != Ipv4Address::UNSPECIFIED && addr != Ipv4Address::BROADCAST && !addr.is_multicast()
}

fn build_dhcp_packet(
    mac: EthernetAddress,
    transaction_id: u32,
    message_type: DhcpMessageType,
    requested_ip: Option<Ipv4Address>,
    server_identifier: Option<Ipv4Address>,
) -> Vec<u8> {
    let dhcp_repr = DhcpRepr {
        message_type,
        transaction_id,
        secs: 0,
        client_hardware_address: mac,
        client_ip: Ipv4Address::UNSPECIFIED,
        your_ip: Ipv4Address::UNSPECIFIED,
        server_ip: Ipv4Address::UNSPECIFIED,
        router: None,
        subnet_mask: None,
        relay_agent_ip: Ipv4Address::UNSPECIFIED,
        broadcast: true,
        requested_ip,
        client_identifier: Some(mac),
        server_identifier,
        parameter_request_list: Some(DHCP_PARAMETER_REQUEST_LIST),
        dns_servers: None,
        max_size: Some(STANDARD_MTU as u16),
        lease_duration: None,
        renew_duration: None,
        rebind_duration: None,
        additional_options: &[],
    };
    let udp_repr = UdpRepr {
        src_port: DHCP_CLIENT_PORT,
        dst_port: DHCP_SERVER_PORT,
    };
    let ipv4_repr = Ipv4Repr {
        src_addr: Ipv4Address::UNSPECIFIED,
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
                .expect("failed to emit DHCP packet")
        },
        &checksum_caps,
    );

    buffer
}
