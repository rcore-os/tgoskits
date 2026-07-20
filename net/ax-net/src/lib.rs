//! Unified network stack for TGOSKits systems.
//!
//! ax-net provides the socket-facing API used by kernels and syscall layers,
//! while delegating TCP/IP protocol mechanics to smoltcp. The crate exposes
//! TCP, UDP, raw IPv4/IPv6 sockets, Unix domain sockets, optional vsock, DNS,
//! DHCP helpers, readiness polling, and interface/control-plane queries.
//!
//! # Architecture
//!
//! The stack intentionally uses one smoltcp `Interface` and one global
//! `SocketSet`. Multiple physical or virtual devices are aggregated below that
//! protocol core by `router::Router`, which acts as a multi-device smoltcp
//! `Device`. This keeps socket ownership, port tables, listen queues, and
//! routing decisions centralized instead of duplicating socket state per NIC.
//!
//! # Polling Model
//!
//! Protocol progress is driven by the dedicated net-poll worker. Socket methods
//! request progress with `request_poll()` and then rely on poll/waker readiness;
//! they must not synchronously drive the whole protocol stack from application
//! hot paths. This preserves the single-owner smoltcp model and avoids lock
//! re-entry between socket operations and interface polling.
//!
//! # Main Modules
//!
//! - `service`: owns the smoltcp interface, net-poll flow, and control plane.
//! - `router`: aggregates devices, route lookup, loopback, and packet queues.
//! - `socket`, `tcp`, `udp`, `raw`: POSIX-like IP socket surface.
//! - `listen_table`, `orphan`, `wrapper`: side tables around smoltcp sockets.
//! - `unix` and `vsock`: local transports outside the smoltcp IP path.

#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;
#[cfg(test)]
extern crate std;

mod addr;
mod blocking;
mod config;
mod consts;
mod device;
mod dhcp_server;
mod general;
mod ip_tos;
mod listen_table;
/// Socket option types and the [`Configurable`](options::Configurable) trait.
pub mod options;
mod orphan;
/// Raw socket implementation.
pub mod raw;
mod router;
mod rx_meta;
mod service;
mod socket;
pub(crate) mod state;
/// TCP socket implementation.
pub mod tcp;
#[cfg(test)]
mod test_runtime;
/// UDP socket implementation.
pub mod udp;
/// Unix domain socket implementation.
pub mod unix;
/// Vsock socket implementation.
#[cfg(feature = "vsock")]
pub mod vsock;
mod wrapper;

use alloc::{
    borrow::ToOwned, boxed::Box, format, string::String, sync::Arc, task::Wake, vec, vec::Vec,
};
use core::{
    net::{IpAddr, Ipv4Addr},
    sync::atomic::{AtomicU64, Ordering},
    task::Waker,
    time::Duration,
};

use ax_errno::{AxError, AxResult, ax_err_type};
use ax_kspin::{PreemptLazy as LazyLock, PreemptOnce as Once};
use ax_sync::SpinMutex;
use ax_task::WaitQueue;
use axpoll::{IoEvents, PollSet};
use smoltcp::{
    socket::dns::{self, GetQueryResultError, StartQueryError},
    wire::{DnsQueryType, EthernetAddress, IpAddress, Ipv4Address, Ipv4Cidr},
};

#[cfg(feature = "vsock")]
pub use self::device::{VsockDevice, VsockDeviceList};
use self::{
    addr::mask_from_prefix,
    device::{EthernetDevice, LoopbackDevice},
    listen_table::ListenTable,
    router::{RouteTable, Router, Rule, SharedRouteTable},
    service::{NetControl, NetInterface, Service, WifiNetworkConfig},
    wrapper::SocketSetWrapper,
};
pub use self::{
    config::{
        DeviceBinding, InterfaceConfig, InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind,
        InterfaceMatcher, Ipv4InterfaceConfig, NetworkConfig, RouteInfo, StaticIpConfig,
    },
    device::{
        ArpEntry, EthernetDeviceList, EthernetDriver, NetDeviceError, NetDeviceResult, NetRxBuffer,
        NetTxBuffer, WifiControl, WifiControlCommand, WifiControlCompletion, WifiControlGeneration,
        WifiControlResult,
    },
    router::NetDevStats,
    socket::{
        CMsgData, IpCmsg, RecvFlags, RecvOptions, SendFlags, SendOptions, Shutdown, Socket,
        SocketAddrEx, SocketOps,
    },
};

static LISTEN_TABLE: LazyLock<ListenTable> = LazyLock::new(ListenTable::new);
static SOCKET_SET: LazyLock<SocketSetWrapper> = LazyLock::new(SocketSetWrapper::new);

static SERVICE: Once<SpinMutex<Service>> = Once::new();
static NET_CONTROL: Once<Arc<NetControl>> = Once::new();
// Monotonic evidence for the single protocol owner. An event racing the
// service/park boundary cannot be erased by an older pass as it could with a
// shared boolean. Device readiness itself comes from runtime-owned PollSets.
static NET_PROTOCOL_EPOCH: AtomicU64 = AtomicU64::new(0);
static NET_POLL_WAKE: WaitQueue = WaitQueue::new();
static NET_POLL_DEVICE_WAKER: LazyLock<Waker> =
    LazyLock::new(|| Waker::from(Arc::new(NetPollWake)));
type DeferredPollEntry = (Arc<PollSet>, IoEvents);
static DEFERRED_POLL_WAKES: LazyLock<SpinMutex<Vec<DeferredPollEntry>>> =
    LazyLock::new(|| SpinMutex::new(Vec::new()));

pub(crate) struct DeferPollWake {
    pub(crate) poll: Arc<PollSet>,
    pub(crate) ready: IoEvents,
}

impl Wake for DeferPollWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // smoltcp invokes socket wakers from the net poll task context after
        // updating readiness. The socket set may still be locked there, so
        // defer the actual PollSet wake to the net worker outer loop.
        defer_poll_wake(self.poll.clone(), self.ready);
    }
}

const DHCP_BOOTSTRAP_ATTEMPTS: usize = 200;
const DHCP_BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn net_poll_device_waker() -> &'static Waker {
    LazyLock::force(&NET_POLL_DEVICE_WAKER)
}

fn get_service() -> ax_sync::SpinMutexGuard<'static, Service> {
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

    validate_config(&config);

    let routes: SharedRouteTable = Arc::new(ax_kspin::SpinRwLock::new(RouteTable::new()));
    let mut router = Router::new(routes.clone());
    let mut interfaces = Vec::new();
    let mut dns = Vec::new();

    let lo_ip = register_loopback(&mut router, &mut interfaces);

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
        let gateway = static_ip.and_then(|cfg| {
            (!cfg.gateway.is_unspecified()).then(|| Ipv4Address::from(cfg.gateway.octets()))
        });
        let dhcp_enabled = cfg.is_none_or(|cfg| cfg.dhcp);
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
        } else if dhcp_enabled {
            dhcp_ifaces.push((id, eth_dev, name.clone(), mac, metric));
            info!("  mode: dhcp");
        } else {
            info!("  mode: none");
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

    ensure_all_interface_configs_used(&config, &used_configs);

    add_default_dns_servers(&config, &mut dns);

    for name in router.device_names() {
        info!("Device: {}", name);
    }
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
    let workers = service.prepare_device_workers();
    workers.register_device_waker(net_poll_device_waker());
    NET_CONTROL.call_once(|| control);
    SERVICE.call_once(|| SpinMutex::new(service));
    workers.start();
    spawn_permanent_worker("net-poll".to_owned(), net_poll_worker)
        .unwrap_or_else(|error| panic!("failed to start net poll worker: {error}"));
    if dhcp_enabled {
        wait_for_dhcp_bootstrap();
    }
}

fn validate_config(config: &NetworkConfig) {
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
}

pub(crate) fn spawn_permanent_worker<F>(name: String, entry: F) -> Result<(), ax_task::TaskError>
where
    F: FnOnce() + Send + 'static,
{
    let handle = ax_task::ThreadBuilder::new(name).spawn(entry)?;
    handle.detach_permanent();
    Ok(())
}

fn register_loopback(router: &mut Router, interfaces: &mut Vec<NetInterface>) -> Ipv4Cidr {
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
    lo_ip
}

fn ensure_all_interface_configs_used(config: &NetworkConfig, used_configs: &[bool]) {
    for (i, used) in used_configs.iter().enumerate() {
        if !used {
            panic!(
                "interface config {} did not match any device",
                config.interfaces[i].name
            );
        }
    }
}

fn add_default_dns_servers(config: &NetworkConfig, dns: &mut Vec<config::DnsServerEntry>) {
    dns.extend(
        config
            .default_dns_servers
            .iter()
            .copied()
            .map(|server| config::DnsServerEntry {
                server: Ipv4Address::from(server.octets()),
                interface_id: InterfaceId::LOOPBACK,
                metric: u32::MAX,
                source: config::DnsSource::Fallback,
            }),
    );
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

fn poll_until_idle() {
    loop {
        let outcome = get_service().poll(&mut SOCKET_SET.inner.lock());
        for waker in outcome.expired_wakers {
            waker.wake();
        }
        if !outcome.progressed {
            return;
        }
    }
}

/// Request network polling.
///
/// This is the lightweight entry used by socket and device paths.
pub fn request_poll() {
    NET_PROTOCOL_EPOCH.fetch_add(1, Ordering::Release);
    NET_POLL_WAKE.notify_one();
}

pub(crate) fn defer_poll_wake(poll: Arc<PollSet>, ready: IoEvents) {
    DEFERRED_POLL_WAKES.lock().push((poll, ready));
    request_poll();
}

fn drain_deferred_poll_wakes() {
    loop {
        let wakes = {
            let mut wakes = DEFERRED_POLL_WAKES.lock();
            if wakes.is_empty() {
                return;
            }
            core::mem::take(&mut *wakes)
        };
        for (poll, ready) in wakes {
            // Readiness was published before the wake was deferred, and no
            // service/socket/device locks are held while draining.
            unsafe { poll.wake(ready) };
        }
    }
}

/// Returns ARP/neighbor entries collected from all devices.
pub fn arp_entries() -> Vec<ArpEntry> {
    get_service().arp_entries()
}

/// Returns per-interface RX/TX byte and packet counters for `/proc/net/dev`.
pub fn net_dev_stats() -> Vec<NetDevStats> {
    get_service().net_dev_stats()
}

/// Returns a snapshot of all configured network interfaces.
pub fn interfaces() -> Vec<InterfaceInfo> {
    get_control().interfaces()
}

/// Looks up an interface snapshot by name.
pub fn interface_by_name(name: &str) -> Option<InterfaceInfo> {
    get_control().interface_by_name(name)
}

/// Looks up an interface snapshot by stable interface id.
pub fn interface_by_id(id: InterfaceId) -> Option<InterfaceInfo> {
    get_control().interface_by_id(id)
}

/// Returns the IPv4 configuration for an interface by name.
pub fn ipv4_config(name: &str) -> Option<Ipv4InterfaceConfig> {
    get_control().ipv4_config(name)
}

/// Assigns a static IPv4 address to an interface at runtime.
pub fn set_interface_ipv4(interface_id: InterfaceId, ip: Ipv4Addr, prefix_len: u8) -> AxResult {
    {
        let mut service = get_service();
        service.configure_static_ipv4(interface_id, Ipv4Address::from(ip.octets()), prefix_len)?;
    }
    request_poll();
    Ok(())
}

/// Removes a configured IPv4 address from an interface at runtime.
pub fn remove_interface_ipv4(interface_id: InterfaceId, ip: Ipv4Addr, prefix_len: u8) -> AxResult {
    {
        let mut service = get_service();
        service.remove_static_ipv4(interface_id, Ipv4Address::from(ip.octets()), prefix_len)?;
    }
    request_poll();
    Ok(())
}

/// Returns public snapshots of configured IPv4 default routes.
pub fn default_routes() -> Vec<RouteInfo> {
    get_control().default_routes()
}

/// Runtime configuration for a statically addressed Ethernet device.
///
/// This is used by drivers that appear after the normal device-probe phase,
/// for example Wi-Fi AP mode devices.
pub struct NetConfig {
    /// Name assigned to the dynamically registered interface.
    pub name: String,
    /// Static IPv4 address.
    pub ip: [u8; 4],
    /// CIDR prefix length.
    pub prefix_len: u8,
    /// If set, enables the built-in one-client DHCP server with this client IP.
    pub dhcp_server_client_ip: Option<[u8; 4]>,
}

/// Registers an extra Ethernet device with a static IPv4 address.
pub fn register_device_with_config(dev: Box<dyn EthernetDriver>, config: NetConfig) {
    let mac = EthernetAddress(dev.mac_address());
    let server_ip = Ipv4Address::from(config.ip);
    let cidr = Ipv4Cidr::new(server_ip, config.prefix_len);
    let eth_dev = EthernetDevice::new(config.name.clone(), dev, Some(cidr));
    let workers = {
        let mut service = get_service();
        let dev_idx = service.register_static_device(config.name.clone(), eth_dev, mac, cidr);
        if let Some(client_ip) = config.dhcp_server_client_ip {
            let client_ip = Ipv4Address::from(client_ip);
            let subnet_mask = mask_from_prefix(config.prefix_len);
            service.enable_dhcp_server(dev_idx, server_ip, client_ip, subnet_mask);
        }
        service.prepare_device_workers_for(dev_idx)
    };
    workers.register_device_waker(net_poll_device_waker());
    workers.start();

    info!("{}: up, mac {mac}, ip {cidr}", config.name);
    request_poll();
}

/// Target role for a runtime Wi-Fi mode switch.
pub enum WifiMode<'a> {
    /// Station: associate to `ssid`/`password`, then use DHCP for addressing.
    Station { ssid: &'a str, password: &'a str },
    /// Open SoftAP on `channel`, static `ip`/`prefix_len`, optionally running a
    /// single-client DHCP server handing out `dhcp_client_ip`.
    AccessPoint {
        ssid: &'a [u8],
        channel: u8,
        ip: [u8; 4],
        prefix_len: u8,
        dhcp_client_ip: Option<[u8; 4]>,
    },
}

/// Atomically switches a wireless interface between STA and SoftAP at runtime.
///
/// This is the single entry point the OS layer (e.g. a StarryOS wireless-
/// extensions `SIOCSIWCOMMIT` handler) calls after staging the desired config.
/// It performs the whole transition in order:
///
/// 1. Drive the link-layer switch through the device's `WifiControl` (the chip
///    driver tears down the old VIF and brings up the new one).
/// 2. Reconfigure this interface's IPv4 / DHCP role in the protocol stack
///    (STA → DHCP client, AP → static IP + optional DHCP server).
///
/// Both halves run from the caller's task context, never from the RX poll
/// task, so the blocking firmware command path cannot deadlock the stack.
///
/// Returns [`AxError::NoSuchDevice`] if `name` has no registered wireless
/// control plane, or [`AxError::Unsupported`] if the link-layer switch fails.
pub fn reconfigure_wifi(name: &str, mode: WifiMode<'_>) -> AxResult<()> {
    let (command, network, expected) = prepare_wifi_reconfiguration(mode)?;
    let control = {
        let service = get_service();
        service
            .wifi_control_by_name(name)
            .ok_or(AxError::NoSuchDevice)?
    };
    let completion = control
        .reconfigure(command)
        .map_err(map_wifi_control_error)?;
    if completion.result != expected {
        return Err(AxError::BadState);
    }
    {
        let mut service = get_service();
        service.reconfigure_wifi_network(name, completion.generation, network)?;
    }
    request_poll();
    Ok(())
}

fn prepare_wifi_reconfiguration(
    mode: WifiMode<'_>,
) -> AxResult<(WifiControlCommand, WifiNetworkConfig, WifiControlResult)> {
    match mode {
        WifiMode::Station { ssid, password } => {
            validate_wifi_ssid(ssid.as_bytes())?;
            if !password.is_empty() && !(8..=63).contains(&password.len()) {
                return Err(AxError::InvalidInput);
            }
            Ok((
                WifiControlCommand::JoinStation {
                    ssid: ssid.as_bytes().to_vec(),
                    passphrase: password.as_bytes().to_vec(),
                },
                WifiNetworkConfig::Station,
                WifiControlResult::StationConnected,
            ))
        }
        WifiMode::AccessPoint {
            ssid,
            channel,
            ip,
            prefix_len,
            dhcp_client_ip,
        } => {
            validate_wifi_ssid(ssid)?;
            if channel == 0 || prefix_len > 32 {
                return Err(AxError::InvalidInput);
            }
            Ok((
                WifiControlCommand::StartAccessPoint {
                    ssid: ssid.to_vec(),
                    channel,
                },
                WifiNetworkConfig::AccessPoint {
                    ip: Ipv4Address::from(ip),
                    prefix_len,
                    dhcp_client_ip: dhcp_client_ip.map(Ipv4Address::from),
                },
                WifiControlResult::AccessPointStarted,
            ))
        }
    }
}

fn validate_wifi_ssid(ssid: &[u8]) -> AxResult<()> {
    if ssid.is_empty() || ssid.len() > 32 {
        Err(AxError::InvalidInput)
    } else {
        Ok(())
    }
}

fn map_wifi_control_error(error: NetDeviceError) -> AxError {
    match error {
        NetDeviceError::Again => AxError::WouldBlock,
        NetDeviceError::BadState => AxError::BadState,
        NetDeviceError::InvalidParam => AxError::InvalidInput,
        NetDeviceError::Io => AxError::Io,
        NetDeviceError::NoMemory => AxError::NoMemory,
        NetDeviceError::Unsupported => AxError::Unsupported,
    }
}

fn next_protocol_delay() -> Option<Duration> {
    let next = {
        let mut service = get_service();
        let sockets = SOCKET_SET.inner.lock();
        service.next_poll_at(&sockets)
    };
    let next = next?;
    let now_micros = ax_hal::time::monotonic_time_nanos() / 1_000;
    let next_micros = next.total_micros().max(0) as u64;
    if next_micros <= now_micros {
        Some(Duration::ZERO)
    } else {
        Some(Duration::from_micros(next_micros - now_micros))
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
        // Producers publish data before incrementing this epoch. An event
        // included in `observed` is therefore visible to this service pass;
        // anything later makes the park predicate true and forces another.
        let observed = NET_PROTOCOL_EPOCH.load(Ordering::Acquire);
        drain_deferred_poll_wakes();
        poll_until_idle();
        drain_deferred_poll_wakes();
        if NET_PROTOCOL_EPOCH.load(Ordering::Acquire) != observed {
            continue;
        }

        match next_protocol_delay() {
            Some(Duration::ZERO) => {
                let _result = ax_task::yield_current_cpu();
            }
            Some(delay) => {
                NET_POLL_WAKE.wait_timeout_until(delay, || {
                    NET_PROTOCOL_EPOCH.load(Ordering::Acquire) != observed
                });
            }
            None => {
                NET_POLL_WAKE.wait_until(|| NET_PROTOCOL_EPOCH.load(Ordering::Acquire) != observed)
            }
        }
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

/// Resolves an A record using the default DNS timeout.
pub fn dns_query(name: &str) -> AxResult<Vec<IpAddr>> {
    dns_query_timeout(name, DNS_DEFAULT_TIMEOUT)
}

/// Resolves an A record using the configured DNS servers and timeout.
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
                    let _result = ax_task::yield_current_cpu();
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

#[cfg(test)]
mod initialization_contract_tests {
    #[test]
    fn device_pollsets_are_initialized_before_service_publication_and_worker_start() {
        let source = include_str!("lib.rs");
        let init_network = source
            .split_once("pub fn init_network")
            .expect("init_network must exist")
            .1
            .split_once("fn validate_config")
            .expect("init_network must precede validate_config")
            .0;
        let initialize_pollsets = init_network
            .find("workers.register_device_waker(net_poll_device_waker())")
            .expect("prepared device poll sets must be initialized before publication");
        let publish_service = init_network
            .find("SERVICE.call_once")
            .expect("the initialized Service must be published once");
        let start_workers = init_network
            .find("workers.start()")
            .expect("prepared device workers must start after publication");

        assert!(
            initialize_pollsets < publish_service && publish_service < start_workers,
            "a runnable worker must never observe or contend with a partially initialized device \
             PollSet"
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
    use std::sync::{Mutex as StdMutex, MutexGuard, Once};

    use ax_sync::SpinMutex;
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
            let routes: SharedRouteTable = Arc::new(ax_kspin::SpinRwLock::new(RouteTable::new()));
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
            SERVICE.call_once(|| SpinMutex::new(service));
        });
    }
}
