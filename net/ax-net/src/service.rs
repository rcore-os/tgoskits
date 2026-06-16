//! Network service and control plane.
//!
//! # Lock Ordering Rules
//!
//! To prevent deadlocks, locks must be acquired from outermost to innermost.
//! Never acquire an outer lock while holding an inner lock.
//!
//! **Lock hierarchy (outermost → innermost):**
//!
//! 1. **SERVICE** (`Mutex<Service>`)
//!    - Outermost, protects entire protocol stack
//!    - Held during `Service::poll()` and waker registration
//!
//! 2. **SOCKET_SET.inner** (`Mutex<SocketSet>`)
//!    - smoltcp socket set (all TCP/UDP/raw/DNS sockets)
//!    - Acquired during poll, socket operations, and state queries
//!    - ⚠️ Never acquire SERVICE while holding this lock
//!
//! 3. **TCP_BOUND_PORTS** (`Mutex<HashMap<u16, Vec<...>>>`)
//!    - Tracks TCP bind() registrations
//!    - Hold duration: registration/unregistration only
//!
//! 4. **Per-port LISTEN_TABLE buckets** (`Arc<Mutex<Vec<ListenTableEntryInner>>>`)
//!    - Innermost, most granular (one mutex per TCP port)
//!
//! **Acquisition order rule:**
//! ```text
//! SERVICE → SOCKET_SET → TCP_BOUND_PORTS → LISTEN_TABLE
//! (outer)                                         (inner)
//! ```
//!
//! # Correct Patterns
//!
//! ```ignore
//! // ✓ Lightweight trigger: socket paths request the dedicated worker.
//! fn socket_operation() {
//!     request_poll()
//! }
//!
//! // ✓ Outer → Inner: SOCKET_SET → LISTEN_TABLE (accept readiness)
//! fn TcpSocket::poll_listener() {
//!     let sockets = SOCKET_SET.inner.lock();
//!     LISTEN_TABLE.can_accept(endpoint, &sockets)
//! }
//! ```
//!
//! # Forbidden Patterns
//!
//! ```ignore
//! // ✗ Inner → Outer: SOCKET_SET → SERVICE (DEADLOCK!)
//! let sockets = SOCKET_SET.inner.lock();
//! get_service().do_something();  // WRONG: reverse order
//!
//! // ✗ Holding any lock while calling wake() (may re-enter via async I/O)
//! let sockets = SOCKET_SET.inner.lock();
//! waker.wake();  // WRONG: potential self-deadlock
//! ```

use alloc::{boxed::Box, format, string::String, vec, vec::Vec};
use core::{
    pin::Pin,
    task::{Context, Waker},
};

use ax_errno::{AxResult, ax_err_type};
use ax_hal::time::{NANOS_PER_MICROS, TimeValue, monotonic_time_nanos, wall_time_nanos};
use ax_task::future::sleep_until;
use smoltcp::{
    iface::{Interface, PollResult, SocketSet},
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
    config::{
        DeviceBinding, DnsServerEntry, DnsSource, InterfaceFlags, InterfaceId, InterfaceInfo,
        InterfaceKind, Ipv4InterfaceConfig, RouteInfo,
    },
    consts::STANDARD_MTU,
    device::{ArpEntry, EthernetDevice},
    dhcp_server::DhcpServer,
    router::{RouteDecision, Router, SharedRouteTable},
};

fn now() -> Instant {
    Instant::from_micros_const((monotonic_time_nanos() / NANOS_PER_MICROS) as i64)
}

use alloc::sync::Arc;

use spin::RwLock;

struct ControlState {
    interfaces: Vec<NetInterface>,
    dns: Vec<DnsServerEntry>,
}

pub struct NetControl {
    state: RwLock<ControlState>,
    pub(crate) routes: SharedRouteTable,
}

impl NetControl {
    pub(crate) fn new(
        interfaces: Vec<NetInterface>,
        routes: SharedRouteTable,
        dns: Vec<DnsServerEntry>,
    ) -> Self {
        Self {
            state: RwLock::new(ControlState { interfaces, dns }),
            routes,
        }
    }

    pub fn dns_servers(&self) -> Vec<Ipv4Address> {
        let state = self.state.read();
        let mut entries = state.dns.clone();
        entries.sort_by_key(|entry| {
            (
                entry.metric,
                entry.interface_id.get(),
                entry.server.octets(),
            )
        });
        let mut servers = Vec::new();
        for entry in entries {
            if !servers.contains(&entry.server) {
                servers.push(entry.server);
            }
        }
        servers
    }

    pub fn interfaces(&self) -> Vec<InterfaceInfo> {
        let state = self.state.read();
        state.interfaces.iter().map(NetInterface::to_info).collect()
    }

    pub fn interface_by_name(&self, name: &str) -> Option<InterfaceInfo> {
        let state = self.state.read();
        state
            .interfaces
            .iter()
            .find(|interface| interface.name == name)
            .map(NetInterface::to_info)
    }

    pub fn interface_by_id(&self, id: InterfaceId) -> Option<InterfaceInfo> {
        let state = self.state.read();
        state
            .interfaces
            .iter()
            .find(|interface| interface.id == id)
            .map(NetInterface::to_info)
    }

    pub fn ipv4_config(&self, name: &str) -> Option<Ipv4InterfaceConfig> {
        let state = self.state.read();
        state
            .interfaces
            .iter()
            .find(|interface| interface.name == name)
            .and_then(|interface| interface.ipv4.map(|address| (interface, address)))
            .map(|(interface, address)| Ipv4InterfaceConfig {
                address,
                gateway: interface.gateway,
            })
    }

    pub fn default_routes(&self) -> Vec<RouteInfo> {
        let _state = self.state.read();
        self.routes.read().default_routes()
    }

    pub fn local_binding_for(&self, endpoint: &IpListenEndpoint) -> AxResult<DeviceBinding> {
        match endpoint.addr {
            Some(addr) => {
                let state = self.state.read();
                let bound_if = state.interfaces.iter().find_map(|interface| {
                    (interface
                        .ipv4
                        .is_some_and(|ipv4| IpAddress::Ipv4(ipv4.address()) == addr))
                    .then_some(interface.id)
                });
                bound_if
                    .map(|interface_id| DeviceBinding {
                        bound_if: Some(interface_id),
                    })
                    .ok_or_else(|| {
                        ax_err_type!(
                            NoSuchDeviceOrAddress,
                            format!("local address {addr} is not assigned to any interface")
                        )
                    })
            }
            None => Ok(DeviceBinding::default()),
        }
    }

    pub fn select_route(&self, dst_addr: &IpAddress) -> AxResult<RouteDecision> {
        self.select_route_with_binding(dst_addr, DeviceBinding::default())
    }

    pub fn select_route_with_binding(
        &self,
        dst_addr: &IpAddress,
        binding: DeviceBinding,
    ) -> AxResult<RouteDecision> {
        let state = self.state.read();
        let routes = self.routes.read();
        let route = routes
            .select_route_if(dst_addr, |interface_id| {
                if binding
                    .bound_if
                    .is_some_and(|bound_if| bound_if != interface_id)
                {
                    return false;
                }
                state
                    .interfaces
                    .iter()
                    .find(|interface| interface.id == interface_id)
                    .is_some_and(|interface| interface.flags.contains(InterfaceFlags::UP))
            })
            .ok_or_else(|| {
                ax_err_type!(
                    NoSuchDeviceOrAddress,
                    format!("no route to destination {dst_addr}")
                )
            })?;
        if let Some(interface) = state
            .interfaces
            .iter()
            .find(|interface| interface.id == route.interface_id)
        {
            debug_assert!(interface.flags.contains(InterfaceFlags::UP));
            debug_assert_eq!(interface.metric, route.metric);
        }
        Ok(route)
    }

    fn commit_interface_update(
        &self,
        update: &NetworkStateUpdate,
        routes: Vec<crate::router::Rule>,
    ) {
        let mut state = self.state.write();
        if let Some(interface) = state
            .interfaces
            .iter_mut()
            .find(|interface| interface.id == update.interface_id)
        {
            interface.ipv4 = update.ipv4;
            interface.gateway = update.gateway;
        }
        state.dns.retain(|entry| {
            entry.interface_id != update.interface_id || entry.source != update.dns_source
        });
        state.dns.extend(
            update
                .dns_servers
                .iter()
                .copied()
                .map(|server| DnsServerEntry {
                    server,
                    interface_id: update.interface_id,
                    metric: update.metric,
                    source: update.dns_source,
                }),
        );
        self.routes
            .write()
            .replace_ipv4_rules_for_interface(update.interface_id, routes);
    }

    fn add_interface(&self, interface: NetInterface, routes: Vec<crate::router::Rule>) {
        self.routes
            .write()
            .replace_ipv4_rules_for_interface(interface.id, routes);
        self.state.write().interfaces.push(interface);
    }

    fn allocate_interface_id(&self) -> InterfaceId {
        let state = self.state.read();
        let next = state
            .interfaces
            .iter()
            .map(|interface| interface.id.get())
            .max()
            .unwrap_or(InterfaceId::LOOPBACK.get())
            .saturating_add(1);
        InterfaceId::new(next)
    }

    fn contains_interface_name(&self, name: &str) -> bool {
        self.state
            .read()
            .interfaces
            .iter()
            .any(|interface| interface.name == name)
    }
}

pub struct Service {
    pub iface: Interface,
    router: Router,
    control: Arc<NetControl>,
    timeout: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    dhcp: Vec<DhcpState>,
    dhcp_server: Option<DhcpServer>,
}

#[derive(Clone)]
pub(crate) struct NetInterface {
    pub id: InterfaceId,
    pub name: String,
    pub kind: InterfaceKind,
    pub mac: Option<EthernetAddress>,
    pub ipv4: Option<Ipv4Cidr>,
    pub gateway: Option<Ipv4Address>,
    pub mtu: usize,
    pub metric: u32,
    pub flags: InterfaceFlags,
}

impl NetInterface {
    fn to_info(&self) -> InterfaceInfo {
        InterfaceInfo {
            id: self.id,
            name: self.name.clone(),
            kind: self.kind,
            mac: self.mac,
            ipv4: self.ipv4.map(|address| Ipv4InterfaceConfig {
                address,
                gateway: self.gateway,
            }),
            mtu: self.mtu,
            flags: self.flags,
            metric: self.metric,
        }
    }
}

struct NetworkStateUpdate {
    interface_id: InterfaceId,
    dev: usize,
    metric: u32,
    old_ipv4: Option<Ipv4Cidr>,
    ipv4: Option<Ipv4Cidr>,
    gateway: Option<Ipv4Address>,
    dns_source: DnsSource,
    dns_servers: Vec<Ipv4Address>,
}

struct DhcpState {
    interface_id: InterfaceId,
    dev: usize,
    ifname: String,
    mac: EthernetAddress,
    metric: u32,
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
const DHCP_MAX_IPV4_HEADER_LEN: usize = 60;
const DHCP_UDP_HEADER_LEN: usize = 8;

impl DhcpState {
    fn new(
        interface_id: InterfaceId,
        dev: usize,
        ifname: String,
        mac: EthernetAddress,
        metric: u32,
    ) -> Self {
        Self {
            interface_id,
            dev,
            ifname,
            mac,
            metric,
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
        interface_id: InterfaceId,
        packet: &[u8],
        timestamp: Instant,
    ) -> Option<DhcpEvent> {
        if interface_id != self.interface_id {
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
                    "{}: DHCP offered address {} from {}",
                    self.ifname,
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
                    interface_id: self.interface_id,
                    dev: self.dev,
                    ifname: self.ifname.clone(),
                    metric: self.metric,
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
                was_configured.then_some(DhcpEvent::Deconfigured {
                    interface_id: self.interface_id,
                    dev: self.dev,
                    ifname: self.ifname.clone(),
                    metric: self.metric,
                })
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
        debug!("{}: DHCP sending {:?}", self.ifname, message_type);

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
    pub fn new(mut router: Router, control: Arc<NetControl>) -> Self {
        let config = smoltcp::iface::Config::new(HardwareAddress::Ip);
        let iface = Interface::new(config, &mut router, now());

        Self {
            iface,
            router,
            control,
            timeout: None,
            dhcp: Vec::new(),
            dhcp_server: None,
        }
    }

    pub fn register_static_device(
        &mut self,
        name: String,
        dev: EthernetDevice,
        mac: EthernetAddress,
        cidr: Ipv4Cidr,
    ) -> usize {
        if self.control.contains_interface_name(&name) {
            panic!("interface name conflict: {}", name);
        }

        let interface_id = self.control.allocate_interface_id();
        let metric = 100;
        let dev = self.router.add_device(interface_id, Box::new(dev));
        let routes = self
            .router
            .ipv4_rules(dev, interface_id, metric, Some(cidr), None);
        Self::set_interface_ipv4(&mut self.iface, None, Some(cidr));
        self.control.add_interface(
            NetInterface {
                id: interface_id,
                name,
                kind: InterfaceKind::Ethernet,
                mac: Some(mac),
                ipv4: Some(cidr),
                gateway: None,
                mtu: STANDARD_MTU,
                metric,
                flags: InterfaceFlags::UP
                    | InterfaceFlags::RUNNING
                    | InterfaceFlags::BROADCAST
                    | InterfaceFlags::MULTICAST,
            },
            routes,
        );
        self.router.start_device_workers(dev);
        dev
    }

    pub fn enable_dhcp(
        &mut self,
        interface_id: InterfaceId,
        dev: usize,
        ifname: String,
        mac: EthernetAddress,
        metric: u32,
    ) {
        self.dhcp.push(DhcpState::new(
            interface_id,
            dev,
            ifname.clone(),
            mac,
            metric,
        ));
        info!("{ifname}: DHCP enabled");
    }

    pub fn dhcp_enabled(&self) -> bool {
        !self.dhcp.is_empty()
    }

    pub fn enable_dhcp_server(
        &mut self,
        dev: usize,
        server_ip: Ipv4Address,
        client_ip: Ipv4Address,
        subnet_mask: Ipv4Address,
    ) {
        let Some(interface_id) = self.router.interface_id_for_dev(dev) else {
            warn!("[dhcp-srv] invalid device index {dev}");
            return;
        };
        self.dhcp_server = Some(DhcpServer::new(
            dev,
            interface_id,
            server_ip,
            client_ip,
            subnet_mask,
        ));
    }

    /// Returns true once DHCP has produced at least one usable interface.
    ///
    /// Startup should not block on every DHCP-enabled NIC: one isolated or
    /// disconnected NIC must not delay unrelated interfaces that are already
    /// routable.
    pub fn dhcp_configured(&self) -> bool {
        self.dhcp.iter().any(|state| state.address.is_some())
    }

    pub fn poll(&mut self, sockets: &mut SocketSet) -> bool {
        let timestamp = now();
        let mut dhcp_events = Vec::new();
        let mut dhcp_server_replies = Vec::new();
        let router_rx_pending;

        {
            let dhcp = &mut self.dhcp;
            let dhcp_server = &mut self.dhcp_server;
            router_rx_pending = self
                .router
                .poll(timestamp, sockets, |interface_id, packet| {
                    for state in dhcp.iter_mut() {
                        if let Some(event) = state.process_packet(interface_id, packet, timestamp) {
                            dhcp_events.push(event);
                        }
                    }
                    if let Some(server) = dhcp_server.as_mut()
                        && let Some(reply) = server.process_packet(interface_id, packet)
                    {
                        dhcp_server_replies.push((server.dev, reply));
                    }
                });
        }
        for event in dhcp_events {
            self.handle_dhcp_event(event);
        }
        let mut dhcp_server_sent = false;
        for (dev, reply) in dhcp_server_replies {
            dhcp_server_sent |= self.router.send_on_device(
                dev,
                IpAddress::Ipv4(Ipv4Address::BROADCAST),
                &reply,
                timestamp,
            );
        }
        let socket_state_changed =
            self.iface.poll(timestamp, &mut self.router, sockets) == PollResult::SocketStateChanged;
        let dhcp_poll_next = self.poll_dhcp(timestamp);

        // Reap orphaned TCP sockets using the SocketSet already held by poll_once().
        crate::orphan::reap_orphans(timestamp, sockets);

        self.router.dispatch(timestamp, sockets)
            || dhcp_poll_next
            || dhcp_server_sent
            || socket_state_changed
            || router_rx_pending
    }

    pub fn next_poll_at(&mut self, sockets: &SocketSet) -> Option<Instant> {
        self.iface.poll_at(now(), sockets)
    }

    fn poll_dhcp(&mut self, timestamp: Instant) -> bool {
        let mut poll_next = false;
        for state in &mut self.dhcp {
            if let Some((dev, next_hop, packet)) = state.poll_packet(timestamp) {
                poll_next |= self
                    .router
                    .send_on_device(dev, next_hop, &packet, timestamp);
            }
        }
        poll_next
    }

    fn handle_dhcp_event(&mut self, event: DhcpEvent) {
        let update = match event {
            DhcpEvent::Configured {
                interface_id,
                dev,
                ifname,
                metric,
                address,
                router,
                dns_servers,
            } => {
                warn!("{ifname}: DHCP acquired address {address}");
                match router {
                    Some(router) => warn!("{ifname}: DHCP router {router}"),
                    None => warn!("{ifname}: DHCP router not provided"),
                }
                for dns in &dns_servers {
                    info!("{ifname}: DHCP DNS {dns}");
                }
                let old_ipv4 = {
                    let Some(state) = self
                        .dhcp
                        .iter_mut()
                        .find(|state| state.interface_id == interface_id)
                    else {
                        return;
                    };
                    let old_ipv4 = state.address;
                    state.address = Some(address);
                    state.dns_servers = dns_servers.clone();
                    old_ipv4
                };
                NetworkStateUpdate {
                    interface_id,
                    dev,
                    metric,
                    old_ipv4,
                    ipv4: Some(address),
                    gateway: router,
                    dns_source: DnsSource::Dhcp,
                    dns_servers,
                }
            }
            DhcpEvent::Deconfigured {
                interface_id,
                dev,
                ifname,
                metric,
            } => {
                let old_ipv4 = {
                    let Some(state) = self
                        .dhcp
                        .iter_mut()
                        .find(|state| state.interface_id == interface_id)
                    else {
                        return;
                    };
                    if state.address.is_some() {
                        info!("{ifname}: DHCP deconfigured");
                    }
                    let old_ipv4 = state.address;
                    state.address = None;
                    state.dns_servers.clear();
                    old_ipv4
                };
                NetworkStateUpdate {
                    interface_id,
                    dev,
                    metric,
                    old_ipv4,
                    ipv4: None,
                    gateway: None,
                    dns_source: DnsSource::Dhcp,
                    dns_servers: Vec::new(),
                }
            }
        };
        self.commit_network_state(update);
    }

    fn commit_network_state(&mut self, update: NetworkStateUpdate) {
        Self::set_interface_ipv4(&mut self.iface, update.old_ipv4, update.ipv4);
        let routes = self.router.ipv4_rules(
            update.dev,
            update.interface_id,
            update.metric,
            update.ipv4,
            update.gateway.map(IpAddress::Ipv4),
        );
        self.control.commit_interface_update(&update, routes);
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

    pub fn arp_entries(&self) -> Vec<ArpEntry> {
        self.router.arp_entries(now())
    }

    pub fn eth0_ipv4_config(&self) -> Option<Ipv4InterfaceConfig> {
        self.control.ipv4_config("eth0")
    }

    pub fn wake_all_devices(&self) {
        self.router.wake_all_devices();
    }

    pub fn register_waker(&mut self, binding: DeviceBinding, waker: &Waker) {
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

        self.router.register_waker(binding, waker);
    }

    pub fn register_device_waker(&mut self, waker: &Waker) {
        self.router.register_device_waker(waker);
    }
}

enum DhcpEvent {
    Configured {
        interface_id: InterfaceId,
        dev: usize,
        ifname: String,
        metric: u32,
        address: Ipv4Cidr,
        router: Option<Ipv4Address>,
        dns_servers: Vec<Ipv4Address>,
    },
    Deconfigured {
        interface_id: InterfaceId,
        dev: usize,
        ifname: String,
        metric: u32,
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
        broadcast: false,
        requested_ip,
        client_identifier: Some(mac),
        server_identifier,
        parameter_request_list: Some(DHCP_PARAMETER_REQUEST_LIST),
        dns_servers: None,
        max_size: Some((STANDARD_MTU - DHCP_MAX_IPV4_HEADER_LEN - DHCP_UDP_HEADER_LEN) as u16),
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

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec::Vec};

    use smoltcp::wire::EthernetAddress;

    use super::*;
    use crate::{device::LoopbackDevice, router::RouteTable};

    #[test]
    fn dhcp_configured_is_true_once_any_interface_has_address() {
        let routes = Arc::new(spin::RwLock::new(RouteTable::new()));
        let mut router = Router::new(routes.clone());
        let dev0 = router.add_device(InterfaceId::new(2), Box::new(LoopbackDevice::new()));
        let dev1 = router.add_device(InterfaceId::new(3), Box::new(LoopbackDevice::new()));
        let control = Arc::new(NetControl::new(Vec::new(), routes, Vec::new()));
        let mut service = Service::new(router, control);

        service.enable_dhcp(
            InterfaceId::new(2),
            dev0,
            "eth0".into(),
            EthernetAddress([0x02, 0, 0, 0, 0, 1]),
            100,
        );
        service.enable_dhcp(
            InterfaceId::new(3),
            dev1,
            "eth1".into(),
            EthernetAddress([0x02, 0, 0, 0, 0, 2]),
            100,
        );
        assert!(!service.dhcp_configured());

        service.dhcp[1].address = Some(Ipv4Cidr::new(Ipv4Address::new(192, 0, 2, 10), 24));
        assert!(service.dhcp_configured());
    }

    #[test]
    fn interface_address_table_handles_loopback_and_two_ethernet_addresses() {
        let routes = Arc::new(spin::RwLock::new(RouteTable::new()));
        let router = Router::new(routes.clone());
        let control = Arc::new(NetControl::new(Vec::new(), routes, Vec::new()));
        let mut service = Service::new(router, control);

        let lo = Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8);
        let eth0 = Ipv4Cidr::new(Ipv4Address::new(10, 0, 2, 15), 24);
        let eth1 = Ipv4Cidr::new(Ipv4Address::new(10, 0, 3, 15), 24);

        service.iface.update_ip_addrs(|ip_addrs| {
            ip_addrs.push(lo.into()).unwrap();
        });
        Service::set_interface_ipv4(&mut service.iface, None, Some(eth0));
        Service::set_interface_ipv4(&mut service.iface, None, Some(eth1));

        assert!(service.iface.ip_addrs().contains(&IpCidr::Ipv4(lo)));
        assert!(service.iface.ip_addrs().contains(&IpCidr::Ipv4(eth0)));
        assert!(service.iface.ip_addrs().contains(&IpCidr::Ipv4(eth1)));
    }
}
