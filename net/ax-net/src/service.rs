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

use alloc::{boxed::Box, format, string::String, sync::Arc, vec, vec::Vec};
use core::task::Waker;

use ax_errno::{AxResult, ax_err_type};
use ax_hal::time::{NANOS_PER_MICROS, monotonic_time_nanos, wall_time_nanos};
use hashbrown::HashMap;
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
use spin::RwLock;

use crate::{
    addr::mask_from_prefix,
    config::{
        DeviceBinding, DnsServerEntry, DnsSource, InterfaceFlags, InterfaceId, InterfaceInfo,
        InterfaceKind, Ipv4InterfaceConfig, RouteInfo,
    },
    consts::STANDARD_MTU,
    device::{ArpEntry, EthernetDevice},
    dhcp_server::{DhcpServer, parse_dhcp_packet},
    router::{RouteDecision, Router, SharedRouteTable},
};

fn now() -> Instant {
    Instant::from_micros_const((monotonic_time_nanos() / NANOS_PER_MICROS) as i64)
}

struct ControlState {
    interfaces: Vec<NetInterface>,
    interface_up: HashMap<InterfaceId, bool>,
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
        let interface_up = interfaces
            .iter()
            .map(|interface| (interface.id, interface.flags.contains(InterfaceFlags::UP)))
            .collect();
        Self {
            state: RwLock::new(ControlState {
                interfaces,
                interface_up,
                dns,
            }),
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
                binding
                    .bound_if
                    .is_none_or(|bound_if| bound_if == interface_id)
                    && state
                        .interface_up
                        .get(&interface_id)
                        .copied()
                        .unwrap_or(false)
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
        let mut is_up = None;
        if let Some(interface) = state
            .interfaces
            .iter_mut()
            .find(|interface| interface.id == update.interface_id)
        {
            interface.ipv4 = update.ipv4;
            interface.gateway = update.gateway;
            if update.up {
                interface
                    .flags
                    .insert(InterfaceFlags::UP | InterfaceFlags::RUNNING);
            } else {
                interface
                    .flags
                    .remove(InterfaceFlags::UP | InterfaceFlags::RUNNING);
            }
            is_up = Some(interface.flags.contains(InterfaceFlags::UP));
        }
        if let Some(is_up) = is_up {
            state.interface_up.insert(update.interface_id, is_up);
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
        let mut state = self.state.write();
        state
            .interface_up
            .insert(interface.id, interface.flags.contains(InterfaceFlags::UP));
        state.interfaces.push(interface);
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
    dhcp: Vec<DhcpState>,
    dhcp_server: Option<DhcpServer>,
    dhcp_events: Vec<DhcpEvent>,
    dhcp_server_replies: Vec<(usize, Vec<u8>)>,
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
    up: bool,
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

enum DhcpPoll {
    Send {
        dev: usize,
        next_hop: IpAddress,
        packet: Vec<u8>,
    },
    Failed(DhcpEvent),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DhcpPhase {
    Discovering,
    Requesting,
    Bound,
    Failed,
}

const DHCP_PARAMETER_REQUEST_LIST: &[u8] = &[1, 3, 6, 42];
const DHCP_MAX_RETRY_SHIFT: usize = 4;
const DHCP_MAX_RETRIES: usize = 8;
const DHCP_FAILED_RETRY_SECS: u64 = 60;
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

        let parsed = parse_dhcp_packet(packet)?;
        if parsed.udp.src_port != DHCP_SERVER_PORT || parsed.udp.dst_port != DHCP_CLIENT_PORT {
            return None;
        }

        if parsed.client_hardware_address != self.mac
            || parsed.transaction_id != self.transaction_id
        {
            return None;
        }

        match (self.phase, parsed.message_type) {
            (DhcpPhase::Discovering, DhcpMessageType::Offer) => {
                if !is_unicast_ipv4(parsed.your_ip) {
                    return None;
                }
                self.offered_address = Some(parsed.your_ip);
                self.server_identifier = parsed.server_identifier.or(Some(parsed.src_addr));
                self.phase = DhcpPhase::Requesting;
                self.retry = 0;
                self.retry_at = timestamp;
                info!(
                    "{}: DHCP offered address {} from {}",
                    self.ifname,
                    parsed.your_ip,
                    self.server_identifier.unwrap_or(parsed.src_addr)
                );
                None
            }
            (DhcpPhase::Requesting, DhcpMessageType::Ack)
            | (DhcpPhase::Bound, DhcpMessageType::Ack) => {
                let subnet_mask = parsed.subnet_mask?;
                let prefix_len = IpAddress::Ipv4(subnet_mask).prefix_len()?;
                if !is_unicast_ipv4(parsed.your_ip) {
                    return None;
                }
                self.phase = DhcpPhase::Bound;
                self.retry = 0;
                let address = Ipv4Cidr::new(parsed.your_ip, prefix_len);
                Some(DhcpEvent::Configured {
                    interface_id: self.interface_id,
                    dev: self.dev,
                    ifname: self.ifname.clone(),
                    metric: self.metric,
                    address,
                    router: parsed.router,
                    dns_servers: parsed.dns_servers,
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

    fn poll_packet(&mut self, timestamp: Instant) -> Option<DhcpPoll> {
        if self.phase == DhcpPhase::Failed && timestamp >= self.retry_at {
            self.reset(timestamp);
        }
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
            DhcpPhase::Bound | DhcpPhase::Failed => return None,
        };

        if self.retry >= DHCP_MAX_RETRIES {
            warn!("{}: DHCP failed after {} retries", self.ifname, self.retry);
            self.phase = DhcpPhase::Failed;
            self.retry_at = timestamp + SmolDuration::from_secs(DHCP_FAILED_RETRY_SECS);
            return Some(DhcpPoll::Failed(DhcpEvent::Deconfigured {
                interface_id: self.interface_id,
                dev: self.dev,
                ifname: self.ifname.clone(),
                metric: self.metric,
            }));
        }
        let retry_delay_secs = 1usize << self.retry.min(DHCP_MAX_RETRY_SHIFT);
        self.retry = self.retry.saturating_add(1);
        self.retry_at = timestamp + SmolDuration::from_secs(retry_delay_secs as u64);
        debug!("{}: DHCP sending {:?}", self.ifname, message_type);

        Some(DhcpPoll::Send {
            dev: self.dev,
            next_hop: IpAddress::Ipv4(Ipv4Address::BROADCAST),
            packet: build_dhcp_packet(
                self.mac,
                self.transaction_id,
                message_type,
                requested_ip,
                server_identifier,
            ),
        })
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
            dhcp: Vec::new(),
            dhcp_server: None,
            dhcp_events: Vec::new(),
            dhcp_server_replies: Vec::new(),
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
        info!("dev {dev}: DHCP server enabled (lease {client_ip})");
    }

    /// Finds the router device index for an interface name such as `wlan0`.
    pub fn device_index(&self, name: &str) -> Option<usize> {
        self.router.device_index(name)
    }

    /// Reconfigures one wireless device as SoftAP: static IPv4 plus optional DHCP server.
    pub fn reconfigure_as_ap(
        &mut self,
        dev: usize,
        server_ip: Ipv4Address,
        prefix_len: u8,
        client_ip: Option<Ipv4Address>,
    ) {
        let Some(interface) = self.interface_for_dev(dev) else {
            warn!("dev {dev}: cannot reconfigure AP for unknown device");
            return;
        };
        let old_ipv4 = self
            .dhcp
            .iter()
            .find(|state| state.dev == dev)
            .and_then(|state| state.address)
            .or(interface.ipv4);
        self.dhcp.retain(|state| state.dev != dev);

        let cidr = Ipv4Cidr::new(server_ip, prefix_len);
        self.commit_network_state(NetworkStateUpdate {
            interface_id: interface.id,
            dev,
            metric: interface.metric,
            old_ipv4,
            ipv4: Some(cidr),
            gateway: None,
            dns_source: DnsSource::Static,
            dns_servers: Vec::new(),
            up: true,
        });

        match client_ip {
            Some(client_ip) => {
                let subnet_mask = mask_from_prefix(prefix_len);
                self.dhcp_server = Some(DhcpServer::new(
                    dev,
                    interface.id,
                    server_ip,
                    client_ip,
                    subnet_mask,
                ));
                info!("dev {dev}: reconfigured as AP {cidr}, DHCP server lease {client_ip}");
            }
            None => {
                self.dhcp_server = None;
                info!("dev {dev}: reconfigured as AP {cidr} (no DHCP server)");
            }
        }
    }

    /// Reconfigures one wireless device as STA and restarts DHCP on it.
    pub fn reconfigure_as_sta(&mut self, dev: usize, mac: EthernetAddress) {
        let Some(interface) = self.interface_for_dev(dev) else {
            warn!("dev {dev}: cannot reconfigure STA for unknown device");
            return;
        };
        if self.dhcp_server.as_ref().is_some_and(|s| s.dev == dev) {
            self.dhcp_server = None;
        }
        self.dhcp.retain(|state| state.dev != dev);
        self.commit_network_state(NetworkStateUpdate {
            interface_id: interface.id,
            dev,
            metric: interface.metric,
            old_ipv4: interface.ipv4,
            ipv4: None,
            gateway: None,
            dns_source: DnsSource::Static,
            dns_servers: Vec::new(),
            up: false,
        });

        self.enable_dhcp(interface.id, dev, interface.name, mac, interface.metric);
        info!("dev {dev}: reconfigured as STA, DHCP client enabled");
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
        let mut dhcp_events = core::mem::take(&mut self.dhcp_events);
        let mut dhcp_server_replies = core::mem::take(&mut self.dhcp_server_replies);
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
        for event in dhcp_events.drain(..) {
            self.handle_dhcp_event(event);
        }
        let mut dhcp_server_sent = false;
        for (dev, reply) in &dhcp_server_replies {
            dhcp_server_sent |= self.router.send_on_device(
                *dev,
                IpAddress::Ipv4(Ipv4Address::BROADCAST),
                reply,
                timestamp,
            );
        }
        dhcp_server_replies.clear();
        self.dhcp_events = dhcp_events;
        self.dhcp_server_replies = dhcp_server_replies;
        let socket_state_changed =
            self.iface.poll(timestamp, &mut self.router, sockets) == PollResult::SocketStateChanged;
        let dhcp_poll_next = self.poll_dhcp(timestamp);

        // Reap orphaned TCP sockets using the SocketSet already held by poll_until_idle().
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
        let mut events = core::mem::take(&mut self.dhcp_events);
        for state in &mut self.dhcp {
            if let Some(poll) = state.poll_packet(timestamp) {
                match poll {
                    DhcpPoll::Send {
                        dev,
                        next_hop,
                        packet,
                    } => {
                        poll_next |= self
                            .router
                            .send_on_device(dev, next_hop, &packet, timestamp);
                    }
                    DhcpPoll::Failed(event) => events.push(event),
                }
            }
        }
        for event in events.drain(..) {
            self.handle_dhcp_event(event);
        }
        self.dhcp_events = events;
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
                    up: true,
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
                    up: false,
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

    fn interface_for_dev(&self, dev: usize) -> Option<NetInterface> {
        let interface_id = self.router.interface_id_for_dev(dev)?;
        self.control
            .state
            .read()
            .interfaces
            .iter()
            .find(|interface| interface.id == interface_id)
            .cloned()
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

    pub fn wake_all_devices(&self) {
        self.router.wake_all_devices();
    }

    pub fn register_waker(&mut self, binding: DeviceBinding, waker: &Waker) {
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
    use crate::{device::LoopbackDevice, router::RouteTableStore};

    #[test]
    fn dhcp_configured_is_true_once_any_interface_has_address() {
        let routes = Arc::new(RouteTableStore::new());
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
        let routes = Arc::new(RouteTableStore::new());
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
