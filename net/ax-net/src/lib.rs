//! Unified network stack for TGOSKits systems.
//!
//! It provides TCP, UDP, raw IPv4/ICMP, Unix domain socket, optional vsock,
//! DNS, DHCP, and readiness primitives on top of [smoltcp] and shared device
//! interfaces.
//!
//! # Organization
//!
//! - [`tcp::TcpSocket`]: TCP socket implementation.
//! - [`udp::UdpSocket`]: UDP socket implementation.
//! - [`raw`]: raw socket support.
//! - [`unix`]: Unix domain socket support.
//!
//! [smoltcp]: https://github.com/smoltcp-rs/smoltcp

#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;
#[cfg(test)]
extern crate std;

mod config;
mod consts;
mod device;
mod general;
mod listen_table;
/// Socket option types and the [`Configurable`](options::Configurable) trait.
pub mod options;
mod orphan;
/// Raw socket implementation.
pub mod raw;
mod router;
mod service;
mod socket;
pub(crate) mod state;
/// TCP socket implementation.
pub mod tcp;
/// UDP socket implementation.
pub mod udp;
/// Unix domain socket implementation.
pub mod unix;
/// Vsock socket implementation.
#[cfg(feature = "vsock")]
pub mod vsock;
mod wrapper;

use alloc::{borrow::ToOwned, boxed::Box, format, sync::Arc, task::Wake, vec, vec::Vec};
use core::{
    net::IpAddr,
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
    time::Duration,
};

use ax_errno::{AxError, AxResult, ax_err_type};
use ax_sync::Mutex;
use ax_task::WaitQueue;
use smoltcp::{
    socket::dns::{self, GetQueryResultError, StartQueryError},
    wire::{DnsQueryType, EthernetAddress, IpAddress, Ipv4Address, Ipv4Cidr},
};
use spin::{LazyLock, Once};

#[cfg(feature = "vsock")]
pub use self::device::{VsockDevice, VsockDeviceList};
pub use self::{
    config::{
        DeviceBinding, InterfaceConfig, InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind,
        InterfaceMatcher, Ipv4InterfaceConfig, NetworkConfig, RouteInfo, StaticIpConfig,
    },
    device::{
        ArpEntry, EthernetDeviceList, EthernetDriver, EthernetIrqAction, EthernetIrqOutcome,
        EthernetIrqRegistrar, EthernetIrqRegistration, EthernetIrqRegistrationError,
        NetDeviceError, NetDeviceResult, NetIrqEvents, NetRxBuffer, NetTxBuffer, RdNetDriver,
        set_ethernet_irq_registrar,
    },
    socket::{
        CMsgData, RecvFlags, RecvOptions, SendFlags, SendOptions, Shutdown, Socket, SocketAddrEx,
        SocketOps,
    },
};
use self::{
    device::{EthernetDevice, LoopbackDevice},
    listen_table::ListenTable,
    router::{RouteTable, Router, Rule, SharedRouteTable},
    service::{NetControl, NetInterface, Service},
    wrapper::SocketSetWrapper,
};

static LISTEN_TABLE: LazyLock<ListenTable> = LazyLock::new(ListenTable::new);
static SOCKET_SET: LazyLock<SocketSetWrapper> = LazyLock::new(SocketSetWrapper::new);

static SERVICE: Once<Mutex<Service>> = Once::new();
static NET_CONTROL: Once<Arc<NetControl>> = Once::new();
static POLLING_INTERFACES: AtomicBool = AtomicBool::new(false);
static POLL_AGAIN: AtomicBool = AtomicBool::new(false);
static NET_POLL_REQUESTED: AtomicBool = AtomicBool::new(false);
static NET_POLL_WAKE: WaitQueue = WaitQueue::new();
static NET_POLL_DEVICE_WAKER: LazyLock<Waker> =
    LazyLock::new(|| Waker::from(Arc::new(NetPollWake)));

const DHCP_BOOTSTRAP_ATTEMPTS: usize = 200;
const DHCP_BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn get_service() -> ax_sync::MutexGuard<'static, Service> {
    SERVICE
        .get()
        .expect("Network service not initialized")
        .lock()
}

pub(crate) fn get_control() -> &'static NetControl {
    NET_CONTROL
        .get()
        .expect("Network service not initialized")
        .as_ref()
}

/// Initializes the network subsystem by NIC devices.
///
/// # Panics
///
/// Panics if called more than once, or if the configuration contains invalid values.
pub fn init_network(mut net_devs: EthernetDeviceList, config: NetworkConfig) {
    if SERVICE.get().is_some() {
        panic!("init_network() called more than once");
    }

    info!("Initialize network subsystem...");

    for cfg in &config.interfaces {
        if cfg.name == "lo" {
            panic!("interface name 'lo' is reserved");
        }
        if cfg.dhcp && cfg.static_ip.is_some() {
            panic!(
                "interface {} has both DHCP and static IP configured",
                cfg.name
            );
        }
        if let Some(static_cfg) = &cfg.static_ip {
            if static_cfg.ip.is_unspecified() {
                panic!("Invalid static IP for {}: unspecified address", cfg.name);
            }
            if static_cfg.prefix_len > 32 {
                panic!("Invalid static IP for {}: prefix length > 32", cfg.name);
            }
            if static_cfg.gateway.is_unspecified() {
                panic!("Invalid gateway for {}: unspecified address", cfg.name);
            }
        }
        for (i, dns) in cfg.dns_servers.iter().enumerate() {
            if dns.is_unspecified() {
                panic!(
                    "Invalid DNS server for {} at index {}: unspecified address",
                    cfg.name, i
                );
            }
        }
    }
    for (i, dns) in config.default_dns_servers.iter().enumerate() {
        if dns.is_unspecified() {
            panic!("Invalid DNS server at index {}: unspecified address", i);
        }
    }

    let routes: SharedRouteTable = Arc::new(spin::RwLock::new(RouteTable::new()));
    let mut router = Router::new(routes.clone());
    let mut interfaces = Vec::new();
    let mut dns = Vec::new();

    let lo_id = InterfaceId::LOOPBACK;
    let lo_dev = router.add_device(lo_id, Box::new(LoopbackDevice::new()));

    let lo_ip = Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8);
    router.add_rule(Rule::new(
        lo_ip.into(),
        None,
        lo_dev,
        lo_id,
        lo_ip.address().into(),
        0,
    ));
    interfaces.push(NetInterface {
        id: lo_id,
        name: "lo".to_owned(),
        kind: InterfaceKind::Loopback,
        mac: None,
        ipv4: Some(lo_ip),
        gateway: None,
        mtu: consts::STANDARD_MTU,
        metric: 0,
        flags: InterfaceFlags::UP | InterfaceFlags::RUNNING | InterfaceFlags::LOOPBACK,
    });

    if net_devs.is_empty() {
        warn!("  No network device found!");
    }

    let mut used_configs = vec![false; config.interfaces.len()];
    let mut dhcp_ifaces = Vec::new();
    let mut eth_ips = Vec::new();

    for (order, dev) in net_devs.drain(..).enumerate() {
        info!("  use NIC {}: {:?}", order, dev.device_name());
        let default_name = format!("eth{}", order);
        let mac = EthernetAddress(dev.mac_address());
        let cfg_idx = find_interface_config(
            &config.interfaces,
            &mut used_configs,
            order,
            mac,
            dev.device_name(),
        );
        let cfg = cfg_idx.map(|idx| &config.interfaces[idx]);
        let name = cfg.map_or(default_name, |cfg| cfg.name.clone());
        if interfaces.iter().any(|interface| interface.name == name) {
            panic!("interface name conflict: {}", name);
        }
        let id = InterfaceId::new((order as u32) + 2);
        let metric = cfg.map_or(100, |cfg| cfg.metric);
        let static_ip = cfg.and_then(|cfg| cfg.static_ip.as_ref());
        let ipv4 =
            static_ip.map(|cfg| Ipv4Cidr::new(Ipv4Address::from(cfg.ip.octets()), cfg.prefix_len));
        let gateway = static_ip.map(|cfg| Ipv4Address::from(cfg.gateway.octets()));
        let eth_dev = router.add_device(id, Box::new(EthernetDevice::new(name.clone(), dev, ipv4)));

        info!("{name}:");
        info!("  id:   {}", id.get());
        info!("  mac:  {}", mac);
        if let Some(ipv4) = ipv4 {
            router.set_ipv4_config(
                eth_dev,
                id,
                metric,
                Some(ipv4),
                gateway.map(IpAddress::Ipv4),
            );
            eth_ips.push(ipv4);
            info!("  mode: static");
            info!("  ip:   {}/{}", ipv4.address(), ipv4.prefix_len());
            if let Some(gateway) = gateway {
                info!("  gw:   {}", gateway);
            }
        } else {
            dhcp_ifaces.push((id, eth_dev, name.clone(), mac, metric));
            info!("  mode: dhcp");
        }
        if let Some(cfg) = cfg {
            dns.extend(
                cfg.dns_servers
                    .iter()
                    .copied()
                    .map(|server| config::DnsServerEntry {
                        server: Ipv4Address::from(server.octets()),
                        interface_id: id,
                        metric,
                        source: config::DnsSource::Static,
                    }),
            );
        }
        interfaces.push(NetInterface {
            id,
            name,
            kind: InterfaceKind::Ethernet,
            mac: Some(mac),
            ipv4,
            gateway,
            mtu: consts::STANDARD_MTU,
            metric,
            flags: InterfaceFlags::UP
                | InterfaceFlags::RUNNING
                | InterfaceFlags::BROADCAST
                | InterfaceFlags::MULTICAST,
        });
    }

    for (i, used) in used_configs.iter().enumerate() {
        if !used {
            panic!(
                "interface config {} did not match any device",
                config.interfaces[i].name
            );
        }
    }

    dns.extend(
        config
            .default_dns_servers
            .iter()
            .copied()
            .map(|server| config::DnsServerEntry {
                server: Ipv4Address::from(server.octets()),
                interface_id: lo_id,
                metric: u32::MAX,
                source: config::DnsSource::Fallback,
            }),
    );

    for name in router.device_names() {
        info!("Device: {}", name);
    }
    router.start_rx_workers();
    router.start_tx_workers();

    let control = Arc::new(NetControl::new(interfaces, routes, dns));
    let mut service = Service::new(router, control.clone());
    service.iface.update_ip_addrs(|ip_addrs| {
        ip_addrs.push(lo_ip.into()).unwrap();
        for ip in eth_ips {
            ip_addrs.push(ip.into()).unwrap();
        }
    });
    for (id, dev, name, mac, metric) in dhcp_ifaces {
        service.enable_dhcp(id, dev, name, mac, metric);
    }
    let dhcp_enabled = service.dhcp_enabled();
    NET_CONTROL.call_once(|| control);
    SERVICE.call_once(|| Mutex::new(service));
    get_service().register_device_waker(&NET_POLL_DEVICE_WAKER);
    ax_task::spawn_with_name(net_poll_worker, "net-poll".to_owned());
    if dhcp_enabled {
        wait_for_dhcp_bootstrap();
    }
}

fn find_interface_config(
    configs: &[InterfaceConfig],
    used: &mut [bool],
    order: usize,
    mac: EthernetAddress,
    driver_name: &str,
) -> Option<usize> {
    let mut matched = None;
    for (idx, cfg) in configs.iter().enumerate() {
        if used[idx] {
            continue;
        }
        let is_match = match &cfg.match_by {
            InterfaceMatcher::ByOrder(expected) => *expected == order,
            InterfaceMatcher::ByMac(expected) => *expected == mac,
            InterfaceMatcher::ByDriverName(expected) => expected == driver_name,
        };
        if is_match {
            if matched.is_some() {
                panic!("multiple interface configs match device {}", driver_name);
            }
            matched = Some(idx);
        }
    }
    if let Some(idx) = matched {
        used[idx] = true;
    }
    matched
}

/// Init vsock subsystem by vsock devices.
#[cfg(feature = "vsock")]
pub fn init_vsock(mut vsock_devs: device::VsockDeviceList) {
    use self::device::register_vsock_device;
    info!("Initialize vsock subsystem...");
    if let Some(dev) = vsock_devs.pop() {
        info!("  use vsock 0: {:?}", dev.name());
        if let Err(e) = register_vsock_device(dev) {
            warn!("Failed to initialize vsock device: {:?}", e);
        }
    } else {
        warn!("  No vsock device found!");
    }
}

fn poll_once() -> bool {
    get_service().poll(&mut SOCKET_SET.inner.lock())
}

fn poll_until_idle() {
    POLL_AGAIN.store(true, Ordering::Release);
    loop {
        if POLLING_INTERFACES
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        while POLL_AGAIN.swap(false, Ordering::AcqRel) {
            while poll_once() {
                #[cfg(not(test))]
                ax_task::yield_now();
            }
        }
        POLLING_INTERFACES.store(false, Ordering::Release);
        if !POLL_AGAIN.load(Ordering::Acquire) {
            return;
        }
    }
}

/// Request network polling from the dedicated net-poll worker.
///
/// This function is retained as a public trigger/debug entry. It no longer
/// synchronously drives the whole protocol stack from the caller's context.
pub fn poll_interfaces() {
    request_poll();
}

pub(crate) fn poll_interfaces_now() {
    poll_until_idle();
}

/// Request network polling.
///
/// This is the lightweight entry used by socket and device paths.
pub fn request_poll() {
    NET_POLL_REQUESTED.store(true, Ordering::Release);
    NET_POLL_WAKE.notify_one(true);
}

pub fn arp_entries() -> Vec<ArpEntry> {
    get_service().arp_entries()
}

pub fn interfaces() -> Vec<InterfaceInfo> {
    get_control().interfaces()
}

pub fn interface_by_name(name: &str) -> Option<InterfaceInfo> {
    get_control().interface_by_name(name)
}

pub fn interface_by_id(id: InterfaceId) -> Option<InterfaceInfo> {
    get_control().interface_by_id(id)
}

pub fn ipv4_config(name: &str) -> Option<Ipv4InterfaceConfig> {
    get_control().ipv4_config(name)
}

pub fn default_routes() -> Vec<RouteInfo> {
    get_control().default_routes()
}

fn next_poll_delay() -> Duration {
    const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(100);
    let next = {
        let mut service = get_service();
        let sockets = SOCKET_SET.inner.lock();
        service.next_poll_at(&sockets)
    };
    let Some(next) = next else {
        return IDLE_POLL_INTERVAL;
    };
    let now_micros = ax_hal::time::monotonic_time_nanos() / 1_000;
    let next_micros = next.total_micros().max(0) as u64;
    if next_micros <= now_micros {
        Duration::ZERO
    } else {
        Duration::from_micros(next_micros - now_micros)
    }
}

struct NetPollWake;

impl Wake for NetPollWake {
    fn wake(self: Arc<Self>) {
        request_poll();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        request_poll();
    }
}

fn net_poll_worker() {
    loop {
        let delay = next_poll_delay();
        let timed_out =
            NET_POLL_WAKE.wait_timeout_until(delay, || NET_POLL_REQUESTED.load(Ordering::Acquire));
        if !timed_out {
            NET_POLL_REQUESTED.store(false, Ordering::Release);
        }
        poll_until_idle();
    }
}

/// Returns the list of configured DNS servers.
///
/// Priority: DHCP-provided servers take precedence over statically configured servers.
/// If DHCP hasn't provided servers, falls back to the servers from `NetworkConfig`.
pub fn dns_servers() -> Vec<Ipv4Address> {
    get_control().dns_servers()
}

const DNS_DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

pub fn dns_query(name: &str) -> AxResult<Vec<IpAddr>> {
    dns_query_timeout(name, DNS_DEFAULT_TIMEOUT)
}

pub fn dns_query_timeout(name: &str, timeout: Duration) -> AxResult<Vec<IpAddr>> {
    let servers = dns_servers();
    if servers.is_empty() {
        return Err(ax_err_type!(NotFound, "no DNS server configured"));
    }

    let servers = servers
        .into_iter()
        .filter(|server| {
            get_control()
                .select_route(&IpAddress::Ipv4(*server))
                .is_ok()
        })
        .map(IpAddress::Ipv4)
        .collect::<Vec<_>>();
    if servers.is_empty() {
        return Err(ax_err_type!(
            NoSuchDeviceOrAddress,
            "no routable DNS server configured"
        ));
    }
    let handle = SOCKET_SET.add(dns::Socket::new(&servers, vec![]));
    DnsSocketGuard(handle).query_timeout(name, DnsQueryType::A, timeout)
}

struct DnsSocketGuard(smoltcp::iface::SocketHandle);

impl DnsSocketGuard {
    fn query_timeout(
        &self,
        name: &str,
        query_type: DnsQueryType,
        timeout: Duration,
    ) -> AxResult<Vec<IpAddr>> {
        let query_handle = {
            let mut service = get_service();
            let mut sockets = SOCKET_SET.inner.lock();
            sockets.get_mut::<dns::Socket>(self.0).start_query(
                service.iface.context(),
                name,
                query_type,
            )
        }
        .map_err(|err| match err {
            StartQueryError::NoFreeSlot => {
                ax_err_type!(ResourceBusy, "DNS query failed: no free slot")
            }
            StartQueryError::InvalidName => {
                ax_err_type!(InvalidInput, "DNS query failed: invalid name")
            }
            StartQueryError::NameTooLong => {
                ax_err_type!(InvalidInput, "DNS query failed: name too long")
            }
        })?;

        let start_time = ax_hal::time::monotonic_time_nanos();
        let timeout_ns = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        let deadline = start_time.saturating_add(timeout_ns);

        loop {
            request_poll();
            match SOCKET_SET.with_socket_mut::<dns::Socket, _, _>(self.0, |socket| {
                socket
                    .get_query_result(query_handle)
                    .map_err(|err| match err {
                        GetQueryResultError::Pending => AxError::WouldBlock,
                        GetQueryResultError::Failed => {
                            ax_err_type!(ConnectionRefused, "DNS query failed")
                        }
                    })
            }) {
                Ok(addrs) => {
                    return Ok(addrs.into_iter().map(IpAddr::from).collect());
                }
                Err(AxError::WouldBlock) => {
                    if ax_hal::time::monotonic_time_nanos() >= deadline {
                        return Err(ax_err_type!(TimedOut, "DNS query timed out"));
                    }
                    ax_task::yield_now();
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl Drop for DnsSocketGuard {
    fn drop(&mut self) {
        SOCKET_SET.remove(self.0);
    }
}

fn wait_for_dhcp_bootstrap() {
    for _ in 0..DHCP_BOOTSTRAP_ATTEMPTS {
        request_poll();
        if get_service().dhcp_configured() {
            return;
        }
        ax_task::sleep(DHCP_BOOTSTRAP_POLL_INTERVAL);
    }
    warn!("DHCP bootstrap timed out");
}

pub(crate) fn endpoint_from_ip_endpoint(
    endpoint: smoltcp::wire::IpEndpoint,
) -> smoltcp::wire::IpListenEndpoint {
    smoltcp::wire::IpListenEndpoint {
        addr: Some(endpoint.addr),
        port: endpoint.port,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
    use std::sync::{Mutex as StdMutex, MutexGuard, Once};

    use ax_sync::Mutex;
    use smoltcp::wire::{IpAddress, Ipv4Address, Ipv4Cidr};

    use crate::{
        NET_CONTROL, SERVICE,
        config::{InterfaceFlags, InterfaceId, InterfaceKind},
        consts::STANDARD_MTU,
        device::LoopbackDevice,
        router::{RouteTable, Router, Rule, SharedRouteTable},
        service::{NetControl, NetInterface, Service},
    };

    pub(crate) const LOCAL_IF: InterfaceId = InterfaceId::new(2);
    pub(crate) const PEER_IF: InterfaceId = InterfaceId::new(3);
    pub(crate) const LOCAL_ADDR: Ipv4Address = Ipv4Address::new(192, 0, 2, 10);
    pub(crate) const PEER_ADDR: Ipv4Address = Ipv4Address::new(198, 51, 100, 20);

    static NETWORK_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    pub(crate) fn network_test_guard() -> MutexGuard<'static, ()> {
        NETWORK_TEST_LOCK.lock().unwrap()
    }

    pub(crate) fn init_split_route_network() {
        static INIT: Once = Once::new();

        INIT.call_once(|| {
            let routes: SharedRouteTable = Arc::new(spin::RwLock::new(RouteTable::new()));
            let mut router = Router::new(routes.clone());
            let local_dev = router.add_device(LOCAL_IF, Box::new(LoopbackDevice::new()));
            let peer_dev = router.add_device(PEER_IF, Box::new(LoopbackDevice::new()));
            let local_cidr = Ipv4Cidr::new(LOCAL_ADDR, 24);
            let peer_cidr = Ipv4Cidr::new(PEER_ADDR, 24);

            router.add_rule(Rule::new(
                local_cidr.into(),
                None,
                local_dev,
                LOCAL_IF,
                IpAddress::Ipv4(LOCAL_ADDR),
                100,
            ));
            router.add_rule(Rule::new(
                peer_cidr.into(),
                None,
                peer_dev,
                PEER_IF,
                IpAddress::Ipv4(PEER_ADDR),
                100,
            ));

            let interfaces = vec![
                NetInterface {
                    id: LOCAL_IF,
                    name: "eth0".into(),
                    kind: InterfaceKind::Ethernet,
                    mac: None,
                    ipv4: Some(local_cidr),
                    gateway: None,
                    mtu: STANDARD_MTU,
                    metric: 100,
                    flags: InterfaceFlags::UP | InterfaceFlags::RUNNING,
                },
                NetInterface {
                    id: PEER_IF,
                    name: "eth1".into(),
                    kind: InterfaceKind::Ethernet,
                    mac: None,
                    ipv4: Some(peer_cidr),
                    gateway: None,
                    mtu: STANDARD_MTU,
                    metric: 100,
                    flags: InterfaceFlags::UP | InterfaceFlags::RUNNING,
                },
            ];

            let control = Arc::new(NetControl::new(interfaces, routes, Vec::new()));
            let mut service = Service::new(router, control.clone());
            service.iface.update_ip_addrs(|ip_addrs| {
                ip_addrs.push(local_cidr.into()).unwrap();
                ip_addrs.push(peer_cidr.into()).unwrap();
            });

            NET_CONTROL.call_once(|| control);
            SERVICE.call_once(|| Mutex::new(service));
        });
    }
}
