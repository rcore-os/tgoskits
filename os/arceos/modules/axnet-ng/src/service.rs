use alloc::{boxed::Box, format, string::String, vec, vec::Vec};
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

use crate::{
    SOCKET_SET,
    consts::{
        DHCP_FAILED_RETRY_INTERVAL, DHCP_MAX_RETRY_COUNT, DHCP_MAX_RETRY_SHIFT,
        DHCP_PARAMETER_REQUEST_LIST, STANDARD_MTU,
    },
    device::ArpEntry,
    router::Router,
};

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
    router: Option<Ipv4Address>,
    dns_servers: Vec<Ipv4Address>,
    lease_duration: Option<SmolDuration>,
    renew_deadline: Option<Instant>,
    rebind_deadline: Option<Instant>,
    lease_acquired_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DhcpPhase {
    Discovering,
    Requesting,
    Bound,
    Renewing,
    Rebinding,
    Failed,
}

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
            router: None,
            dns_servers: Vec::new(),
            lease_duration: None,
            renew_deadline: None,
            rebind_deadline: None,
            lease_acquired_at: None,
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
            (DhcpPhase::Discovering | DhcpPhase::Failed, DhcpMessageType::Offer) => {
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
            (
                DhcpPhase::Requesting | DhcpPhase::Bound | DhcpPhase::Failed,
                DhcpMessageType::Ack,
            )
            | (DhcpPhase::Renewing | DhcpPhase::Rebinding, DhcpMessageType::Ack) => {
                let subnet_mask = dhcp_repr.subnet_mask?;
                let prefix_len = IpAddress::Ipv4(subnet_mask).prefix_len()?;
                if !is_unicast_ipv4(dhcp_repr.your_ip) {
                    return None;
                }
                self.phase = DhcpPhase::Bound;
                self.retry = 0;
                // Update server_identifier on every ACK so Renewing unicast
                // targets the correct server across failover / renumbering.
                self.server_identifier = dhcp_repr.server_identifier.or(Some(ipv4_repr.src_addr));
                let address = Ipv4Cidr::new(dhcp_repr.your_ip, prefix_len);
                Some(DhcpEvent::Configured {
                    address,
                    router: dhcp_repr.router,
                    dns_servers: dhcp_repr
                        .dns_servers
                        .as_ref()
                        .map(|servers| servers.iter().copied().collect())
                        .unwrap_or_default(),
                    lease_duration: dhcp_repr
                        .lease_duration
                        .map(|s| SmolDuration::from_secs(s as u64)),
                    renew_duration: dhcp_repr
                        .renew_duration
                        .map(|s| SmolDuration::from_secs(s as u64)),
                    rebind_duration: dhcp_repr
                        .rebind_duration
                        .map(|s| SmolDuration::from_secs(s as u64)),
                })
            }
            (_, DhcpMessageType::Nak) => {
                let old_address = self.address;
                self.reset(timestamp);
                old_address.map(|addr| DhcpEvent::Deconfigured {
                    old_address: Some(addr),
                })
            }
            _ => None,
        }
    }

    fn poll_packet(&mut self, timestamp: Instant) -> Option<(usize, IpAddress, Vec<u8>)> {
        // Phase transitions are time-driven, not gated by retry_at.
        if self.phase == DhcpPhase::Bound {
            if let Some(deadline) = self.rebind_deadline
                && timestamp >= deadline
            {
                self.phase = DhcpPhase::Rebinding;
                self.retry = 0;
                self.retry_at = timestamp;
                info!("eth0: DHCP entering rebinding phase");
            } else if let Some(deadline) = self.renew_deadline
                && timestamp >= deadline
            {
                self.phase = DhcpPhase::Renewing;
                self.retry = 0;
                self.retry_at = timestamp;
                info!("eth0: DHCP entering renew phase");
            }
        } else if self.phase == DhcpPhase::Renewing
            && let Some(deadline) = self.rebind_deadline
            && timestamp >= deadline
        {
            self.phase = DhcpPhase::Rebinding;
            self.retry = 0;
            self.retry_at = timestamp;
            info!("eth0: DHCP entering rebinding phase");
        }

        if self.phase != DhcpPhase::Failed
            && self.phase != DhcpPhase::Bound
            && self.phase != DhcpPhase::Renewing
            && self.phase != DhcpPhase::Rebinding
            && self.retry >= DHCP_MAX_RETRY_COUNT
        {
            self.phase = DhcpPhase::Failed;
            self.retry_at = timestamp + SmolDuration::from_secs(DHCP_FAILED_RETRY_INTERVAL);
            warn!(
                "eth0: DHCP failed after {} attempts, retrying every {}s",
                DHCP_MAX_RETRY_COUNT, DHCP_FAILED_RETRY_INTERVAL
            );
            return None;
        }

        // retry_at only gates packet transmission, not phase transitions.
        if timestamp < self.retry_at {
            return None;
        }

        let (message_type, requested_ip, server_identifier, dst_addr, broadcast) = match self.phase
        {
            DhcpPhase::Discovering => (
                DhcpMessageType::Discover,
                None,
                None,
                Ipv4Address::BROADCAST,
                true,
            ),
            DhcpPhase::Requesting => (
                DhcpMessageType::Request,
                self.offered_address,
                self.server_identifier,
                Ipv4Address::BROADCAST,
                true,
            ),
            // RFC 2131 4.4.5: Renewing uses ciaddr (not requested_ip), no
            // server_identifier option, unicast to the server.
            DhcpPhase::Renewing => (
                DhcpMessageType::Request,
                None,
                None,
                self.server_identifier.unwrap_or(Ipv4Address::BROADCAST),
                false,
            ),
            // RFC 2131 4.4.5: Rebinding uses ciaddr (not requested_ip), no
            // server_identifier, broadcast.
            DhcpPhase::Rebinding => (
                DhcpMessageType::Request,
                None,
                None,
                Ipv4Address::BROADCAST,
                true,
            ),
            DhcpPhase::Failed => (
                DhcpMessageType::Discover,
                None,
                None,
                Ipv4Address::BROADCAST,
                true,
            ),
            DhcpPhase::Bound => return None,
        };

        let client_ip = self
            .address
            .map(|cidr| cidr.address())
            .unwrap_or(Ipv4Address::UNSPECIFIED);

        let retry_at = if self.phase == DhcpPhase::Failed {
            info!("eth0: DHCP retrying in failed state");
            timestamp + SmolDuration::from_secs(DHCP_FAILED_RETRY_INTERVAL)
        } else if self.phase == DhcpPhase::Renewing {
            // RFC 2131 4.4.5: divide remaining time until T2 into equal
            // intervals so retransmissions are spread across the window
            // instead of clustering at the start.
            let remaining = self
                .rebind_deadline
                .map(|d| (d - timestamp).total_micros() / 1_000_000)
                .map(|s| s.max(1))
                .unwrap_or(360);
            let interval = (remaining / 6).clamp(15, 3600);
            self.retry = self.retry.saturating_add(1);
            timestamp + SmolDuration::from_secs(interval)
        } else if self.phase == DhcpPhase::Rebinding {
            let remaining = self
                .lease_expiry()
                .map(|d| (d - timestamp).total_micros() / 1_000_000)
                .map(|s| s.max(1))
                .unwrap_or(360);
            let interval = (remaining / 6).clamp(15, 3600);
            self.retry = self.retry.saturating_add(1);
            timestamp + SmolDuration::from_secs(interval)
        } else {
            let delay = retry_delay_secs(self.retry, self.transaction_id, timestamp);
            self.retry = self.retry.saturating_add(1);
            timestamp + SmolDuration::from_secs(delay)
        };
        self.retry_at = retry_at;

        Some((
            self.dev,
            IpAddress::Ipv4(dst_addr),
            build_dhcp_packet(
                self.mac,
                self.transaction_id,
                message_type,
                requested_ip,
                server_identifier,
                client_ip,
                dst_addr,
                broadcast,
            ),
        ))
    }

    fn lease_expiry(&self) -> Option<Instant> {
        let acquired = self.lease_acquired_at?;
        let duration = self.lease_duration?;
        Some(Instant::from_micros_const(
            acquired.total_micros() + duration.total_micros() as i64,
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
        self.router = None;
        self.dns_servers.clear();
        self.lease_duration = None;
        self.renew_deadline = None;
        self.rebind_deadline = None;
        self.lease_acquired_at = None;
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

        // Check lease expiry before processing packets so that Deconfigured
        // fires before any potential Configured from the same poll cycle.
        if let Some(ref mut state) = self.dhcp
            && let (Some(acquired), Some(duration)) =
                (state.lease_acquired_at, state.lease_duration)
            && timestamp >= acquired + duration
        {
            let old_address = state.address;
            if old_address.is_some() {
                dhcp_events.push(DhcpEvent::Deconfigured { old_address });
            }
            info!("eth0: DHCP lease expired");
            state.reset(timestamp);
        }

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
                lease_duration,
                renew_duration,
                rebind_duration,
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
                state.router = router;
                state.dns_servers = dns_servers;
                self.router
                    .set_ipv4_config(state.dev, Some(address), router.map(IpAddress::Ipv4));

                // Set up lease timers for T1 (renew) and T2 (rebind).
                let now = now();
                state.lease_acquired_at = Some(now);
                state.lease_duration = lease_duration;
                if let Some(dur) = lease_duration {
                    // Per RFC 2131: T1 defaults to 0.5 * lease, T2 defaults to 0.875 * lease.
                    let t1 = renew_duration.unwrap_or(dur / 2);
                    let t2 = rebind_duration.unwrap_or(dur * 7 / 8);
                    state.renew_deadline = Some(now + t1);
                    state.rebind_deadline = Some(now + t2);
                }
            }
            DhcpEvent::Deconfigured { old_address } => {
                let Some(state) = &mut self.dhcp else {
                    return;
                };
                if old_address.is_some() {
                    info!("eth0: DHCP deconfigured");
                }
                Self::set_interface_ipv4(&mut self.iface, old_address, None);
                // reset() already cleared state.address / state.dns_servers
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

    pub fn arp_entries(&self) -> Vec<ArpEntry> {
        self.router.arp_entries(now())
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

    pub fn dhcp_poll_duration(&self) -> Option<SmolDuration> {
        let dhcp = self.dhcp.as_ref()?;
        let now = now();
        if dhcp.phase == DhcpPhase::Bound {
            // Return the nearest of renew_deadline, rebind_deadline, or lease expiry
            // so the wake mechanism can advance the state machine past T1/T2.
            let now_us = now.total_micros();
            let deadlines: [Option<i64>; 3] = [
                dhcp.renew_deadline.map(|d| d.total_micros()),
                dhcp.rebind_deadline.map(|d| d.total_micros()),
                dhcp.lease_expiry().map(|d| d.total_micros()),
            ];
            let next_us = deadlines.iter().filter_map(|&d| d).min()?;
            if next_us > now_us {
                return Some(SmolDuration::from_micros((next_us - now_us) as u64));
            } else {
                return Some(SmolDuration::from_micros(0));
            }
        }
        if dhcp.retry_at > now {
            Some(dhcp.retry_at - now)
        } else {
            Some(SmolDuration::from_micros(0))
        }
    }

    pub fn dhcp_info(&self) -> Option<String> {
        let state = self.dhcp.as_ref()?;
        let addr = state.address?;
        let mut info = format!("ip={}/{}\n", addr.address(), addr.prefix_len());
        if let Some(gw) = state.router {
            info.push_str(&format!("gateway={}\n", gw));
        }
        for dns in &state.dns_servers {
            info.push_str(&format!("dns={}\n", dns));
        }
        Some(info)
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
        lease_duration: Option<SmolDuration>,
        renew_duration: Option<SmolDuration>,
        rebind_duration: Option<SmolDuration>,
    },
    Deconfigured {
        old_address: Option<Ipv4Cidr>,
    },
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

/// RFC 2131 4.1: retransmission MUST use randomized exponential backoff.
/// Returns a jittered delay in seconds.
fn retry_delay_secs(retry: usize, transaction_id: u32, timestamp: Instant) -> u64 {
    let base = 1u64 << retry.min(DHCP_MAX_RETRY_SHIFT);
    // SplitMix-style hash — good enough for ±50% jitter without an RNG crate.
    let mut h = transaction_id as u64;
    h = h.wrapping_mul(0x9e3779b97f4a7c15);
    h ^= retry as u64;
    h = h.wrapping_mul(0x9e3779b97f4a7c15);
    h ^= timestamp.total_micros() as u64;
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58476d1ce4e5b9);
    h ^= h >> 27;
    let jitter = (base / 2).max(1);
    let offset = h % (jitter * 2 + 1);
    base.saturating_add(offset).saturating_sub(jitter)
}

#[allow(clippy::too_many_arguments)]
fn build_dhcp_packet(
    mac: EthernetAddress,
    transaction_id: u32,
    message_type: DhcpMessageType,
    requested_ip: Option<Ipv4Address>,
    server_identifier: Option<Ipv4Address>,
    client_ip: Ipv4Address,
    dst_addr: Ipv4Address,
    broadcast: bool,
) -> Vec<u8> {
    let dhcp_repr = DhcpRepr {
        message_type,
        transaction_id,
        secs: 0,
        client_hardware_address: mac,
        client_ip,
        your_ip: Ipv4Address::UNSPECIFIED,
        server_ip: Ipv4Address::UNSPECIFIED,
        router: None,
        subnet_mask: None,
        relay_agent_ip: Ipv4Address::UNSPECIFIED,
        broadcast,
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
        src_addr: client_ip,
        dst_addr,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(phase: DhcpPhase, address: Option<Ipv4Cidr>) -> DhcpState {
        DhcpState {
            dev: 0,
            mac: EthernetAddress([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]),
            transaction_id: 42,
            phase,
            retry_at: Instant::from_micros_const(0),
            retry: 0,
            offered_address: None,
            server_identifier: None,
            address,
            router: None,
            dns_servers: Vec::new(),
            lease_duration: None,
            renew_deadline: None,
            rebind_deadline: None,
            lease_acquired_at: None,
        }
    }

    #[test]
    fn bound_returns_none() {
        let mut state = make_state(DhcpPhase::Bound, None);
        let t = Instant::from_micros_const(1_000_000);
        assert!(state.poll_packet(t).is_none());
        assert!(
            state
                .poll_packet(t + SmolDuration::from_secs(3600))
                .is_none()
        );
    }

    #[test]
    fn discovering_sends_discover() {
        let mut state = make_state(DhcpPhase::Discovering, None);
        let result = state.poll_packet(Instant::from_micros_const(0));
        assert!(result.is_some());
        let (dev, ip, _packet) = result.unwrap();
        assert_eq!(dev, 0);
        assert_eq!(ip, IpAddress::Ipv4(Ipv4Address::BROADCAST));
    }

    #[test]
    fn retry_count_enters_failed() {
        let mut state = make_state(DhcpPhase::Discovering, None);
        let mut t = Instant::from_micros_const(0);

        for i in 0..DHCP_MAX_RETRY_COUNT {
            let result = state.poll_packet(t);
            assert!(result.is_some(), "should send Discover at attempt {i}");
            assert_eq!(state.phase, DhcpPhase::Discovering);
            t = state.retry_at;
        }

        // retry == DHCP_MAX_RETRY_COUNT → enter Failed
        let result = state.poll_packet(t);
        assert!(result.is_none(), "should return None when entering Failed");
        assert_eq!(state.phase, DhcpPhase::Failed);
    }

    #[test]
    fn failed_state_retries_fixed_interval() {
        let mut state = make_state(DhcpPhase::Failed, None);
        let t = Instant::from_micros_const(0);

        // Immediately sends a Discover and sets retry_at = t + 60s
        let result = state.poll_packet(t);
        assert!(result.is_some(), "Failed should send Discover immediately");
        assert_eq!(
            state.retry_at,
            t + SmolDuration::from_secs(DHCP_FAILED_RETRY_INTERVAL)
        );

        // Before the 60s interval, nothing is sent
        let result = state.poll_packet(t + SmolDuration::from_secs(30));
        assert!(result.is_none(), "should wait for 60s interval");

        // At the 60s mark, sends another Discover
        let result = state.poll_packet(t + SmolDuration::from_secs(60));
        assert!(result.is_some(), "should retry at 60s interval");
    }

    #[test]
    fn bound_to_renewing_transition() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let acquired = Instant::from_micros_const(0);
        let mut state = make_state(DhcpPhase::Bound, Some(addr));
        state.lease_acquired_at = Some(acquired);
        state.lease_duration = Some(SmolDuration::from_secs(100));
        state.renew_deadline = Some(acquired + SmolDuration::from_secs(50));
        state.rebind_deadline = Some(acquired + SmolDuration::from_secs(87));

        // After T1 (50s) but before T2 (87s)
        let result = state.poll_packet(acquired + SmolDuration::from_secs(60));
        assert_eq!(state.phase, DhcpPhase::Renewing);
        assert!(result.is_some(), "Renewing should send unicast Request");
    }

    #[test]
    fn renewing_to_rebinding_transition() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let acquired = Instant::from_micros_const(0);
        let mut state = make_state(DhcpPhase::Renewing, Some(addr));
        state.lease_acquired_at = Some(acquired);
        state.lease_duration = Some(SmolDuration::from_secs(100));
        state.renew_deadline = Some(acquired + SmolDuration::from_secs(50));
        state.rebind_deadline = Some(acquired + SmolDuration::from_secs(87));

        // Before T2 (87s), stays in Renewing
        let _result = state.poll_packet(acquired + SmolDuration::from_secs(80));
        assert_eq!(state.phase, DhcpPhase::Renewing);

        // After T2, transitions to Rebinding
        let result = state.poll_packet(acquired + SmolDuration::from_secs(90));
        assert_eq!(state.phase, DhcpPhase::Rebinding);
        assert!(result.is_some(), "Rebinding should send broadcast Request");
    }

    #[test]
    fn lease_expiry_is_handled_at_service_level() {
        // Lease expiry cleanup (iface/router) is done in Service::poll(),
        // not in poll_packet(). poll_packet() on a stale Bound lease that has
        // no deadline fields set will simply return None.
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let acquired = Instant::from_micros_const(0);
        let mut state = make_state(DhcpPhase::Bound, Some(addr));
        state.lease_acquired_at = Some(acquired);
        state.lease_duration = Some(SmolDuration::from_secs(100));

        // Before expiry, Bound returns None
        let _result = state.poll_packet(acquired + SmolDuration::from_secs(90));
        assert_eq!(state.phase, DhcpPhase::Bound);

        // After expiry, poll_packet doesn't auto-reset (Service handles it).
        let result = state.poll_packet(acquired + SmolDuration::from_secs(101));
        assert_eq!(state.phase, DhcpPhase::Bound);
        assert!(
            result.is_none(),
            "poll_packet does not reset on lease expiry"
        );

        // But lease_expiry() still reports the correct expiry timestamp.
        assert!(state.lease_expiry().is_some());
    }

    #[test]
    fn reset_clears_dhcp_state() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let mut state = make_state(DhcpPhase::Bound, Some(addr));
        state.dns_servers = vec![Ipv4Address::new(10, 0, 2, 3)];
        state.lease_duration = Some(SmolDuration::from_secs(3600));
        state.lease_acquired_at = Some(Instant::from_micros_const(0));
        state.renew_deadline = Some(Instant::from_micros_const(1_800_000_000));
        state.rebind_deadline = Some(Instant::from_micros_const(3_150_000_000));

        state.reset(Instant::from_micros_const(2_000_000_000));

        assert_eq!(state.phase, DhcpPhase::Discovering);
        assert_eq!(state.retry, 0);
        assert!(state.address.is_none());
        assert!(state.dns_servers.is_empty());
        assert!(state.lease_duration.is_none());
        assert!(state.renew_deadline.is_none());
        assert!(state.rebind_deadline.is_none());
        assert!(state.lease_acquired_at.is_none());
    }

    #[test]
    fn exponential_backoff_with_jitter() {
        let mut state = make_state(DhcpPhase::Discovering, None);

        for i in 0..DHCP_MAX_RETRY_SHIFT + 1 {
            let before = state.retry_at;
            let r = state.poll_packet(before);
            assert!(r.is_some(), "retry {i} should send a packet");
            let after = state.retry_at;
            let delay = (after - before).total_micros() / 1_000_000;

            // Jitter range is ±50% of the base, base = 2^i (capped at 16).
            let base = 1u64 << i.min(DHCP_MAX_RETRY_SHIFT);
            let jitter = (base / 2).max(1);
            let max = base + jitter;
            let min = base.saturating_sub(jitter);
            assert!(
                delay >= min && delay <= max,
                "retry {i}: delay {delay}s out of range [{min}, {max}]"
            );
        }
    }

    /// Verify that `lease_expiry()` computes the correct expiry instant.
    #[test]
    fn lease_expiry_computes_correct_timestamp() {
        let mut state = make_state(DhcpPhase::Bound, None);
        let acquired = Instant::from_micros_const(1_000_000);
        state.lease_acquired_at = Some(acquired);
        state.lease_duration = Some(SmolDuration::from_secs(3600));
        let expiry = state.lease_expiry().unwrap();
        assert_eq!(
            expiry.total_micros(),
            acquired.total_micros() + 3_600_000_000i64
        );
    }

    /// Scan the DHCP options area for a given tag. Returns the value slice.
    fn find_option(packet: &[u8], tag: u8) -> Option<Vec<u8>> {
        // Skip IPv4(20) + UDP(8) + BOOTP header(236) + magic cookie(4) = 268
        let mut pos = 268;
        while pos + 2 <= packet.len() {
            let t = packet[pos];
            if t == 255 {
                break;
            }
            let len = packet[pos + 1] as usize;
            if t == tag {
                return Some(packet[pos + 2..pos + 2 + len].to_vec());
            }
            pos += 2 + len;
        }
        None
    }

    fn dhcp_ciaddr(packet: &[u8]) -> Ipv4Address {
        Ipv4Address::new(packet[40], packet[41], packet[42], packet[43])
    }

    #[test]
    fn renewing_packet_rfc2131_4_4_5() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let mut state = make_state(DhcpPhase::Renewing, Some(addr));
        state.server_identifier = Some(Ipv4Address::new(10, 0, 2, 2));
        let t = Instant::from_micros_const(0);

        let (_, dst, pkt) = state.poll_packet(t).unwrap();
        // Unicast to the server
        assert_eq!(dst, IpAddress::Ipv4(Ipv4Address::new(10, 0, 2, 2)));
        // ciaddr MUST be the client's IP
        assert_eq!(
            dhcp_ciaddr(&pkt),
            Ipv4Address::new(10, 0, 2, 15),
            "Renewing ciaddr must be client IP"
        );
        // MUST NOT carry requested_ip (option 50)
        assert!(
            find_option(&pkt, 50).is_none(),
            "Renewing must not include requested_ip"
        );
        // MUST NOT carry server_identifier (option 54)
        assert!(
            find_option(&pkt, 54).is_none(),
            "Renewing must not include server_identifier"
        );
    }

    #[test]
    fn rebinding_packet_rfc2131_4_4_5() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let mut state = make_state(DhcpPhase::Rebinding, Some(addr));
        let t = Instant::from_micros_const(0);

        let (_, dst, pkt) = state.poll_packet(t).unwrap();
        // Broadcast
        assert_eq!(dst, IpAddress::Ipv4(Ipv4Address::BROADCAST));
        // ciaddr MUST be the client's IP
        assert_eq!(
            dhcp_ciaddr(&pkt),
            Ipv4Address::new(10, 0, 2, 15),
            "Rebinding ciaddr must be client IP"
        );
        // MUST NOT carry requested_ip (option 50)
        assert!(
            find_option(&pkt, 50).is_none(),
            "Rebinding must not include requested_ip"
        );
        // MUST NOT carry server_identifier (option 54)
        assert!(
            find_option(&pkt, 54).is_none(),
            "Rebinding must not include server_identifier"
        );
    }

    #[test]
    fn renewing_retransmission_is_linear_not_exponential() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let t0 = Instant::from_micros_const(1_800_000_000);
        let mut state = make_state(DhcpPhase::Renewing, Some(addr));
        state.lease_acquired_at = Some(t0);
        state.lease_duration = Some(SmolDuration::from_secs(3600));
        state.rebind_deadline = Some(t0 + SmolDuration::from_secs(1350));

        // First retransmit: interval ≈ remaining / 6 = 1350 / 6 = 225s
        let result = state.poll_packet(t0);
        assert!(result.is_some());
        let first_interval = (state.retry_at - t0).total_micros() / 1_000_000;
        assert!(
            first_interval > 60,
            "Renewing first retry should be based on remaining/T2 window, not 1s exponential; got \
             {first_interval}s"
        );

        // Second retransmit: interval shrinks because remaining time is less,
        // but should STILL be a linear fraction, not exponential doubling.
        let t1 = state.retry_at;
        let result = state.poll_packet(t1);
        assert!(result.is_some());
        let second_interval = (state.retry_at - t1).total_micros() / 1_000_000;
        // If exponential, second would be ~2x first. Linear should be ≤ first.
        assert!(
            second_interval <= first_interval && second_interval > 60,
            "linear: second interval {second_interval}s should be ≤ first {first_interval}s, not \
             doubled"
        );
    }

    #[test]
    fn rebinding_retransmission_is_linear_not_exponential() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let t0 = Instant::from_micros_const(3_150_000_000);
        let mut state = make_state(DhcpPhase::Rebinding, Some(addr));
        state.lease_acquired_at = Some(Instant::from_micros_const(0));
        // lease = 3600s, t0 = 3150s into lease, 450s remaining
        state.lease_duration = Some(SmolDuration::from_secs(3600));

        let result = state.poll_packet(t0);
        assert!(result.is_some());
        let interval = (state.retry_at - t0).total_micros() / 1_000_000;
        // remaining = 450s, /6 = 75s
        assert!(
            (15..=90).contains(&interval),
            "Rebinding retry interval based on remaining lease; got {interval}s"
        );
    }

    /// When both renew_deadline and rebind_deadline are set, `lease_expiry()`
    /// should return the lease-expiry moment (not confuse it with the deadlines).
    #[test]
    fn lease_expiry_not_confused_with_deadlines() {
        let addr = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let acquired = Instant::from_micros_const(0);
        let mut state = make_state(DhcpPhase::Bound, Some(addr));
        state.lease_acquired_at = Some(acquired);
        state.lease_duration = Some(SmolDuration::from_secs(3600));
        // renew at 1800s, rebind at 3150s — lease_expiry should still be 3600s
        let expiry = state.lease_expiry().unwrap();
        assert_eq!(
            expiry.total_micros(),
            acquired.total_micros() + 3_600_000_000i64
        );
    }
}
