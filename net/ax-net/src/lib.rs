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
//! # Execution Model
//!
//! A unique CPU-pinned protocol executor owns every smoltcp poll. Socket methods
//! publish generations with `request_poll()` and then rely on poll/waker
//! readiness; they never synchronously become a second protocol owner. Separate
//! CPU-pinned queue executors own hard-IRQ continuation, DMA reclaim/refill, and
//! bounded queue polling. Preallocated SPSC rings transfer move-only frame tokens
//! between those two ownership domains.
//!
//! # Main Modules
//!
//! - `service`: owns the smoltcp interface and control plane.
//! - `poll_runtime`: owns generation-based protocol scheduling.
//! - `queue_runtime`: owns IRQ affinity domains and queue executors.
//! - `router`: aggregates protocol ports, route lookup, and loopback.
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
mod config;
mod consts;
mod device;
mod dhcp_server;
mod error;
mod general;
mod ip_tos;
mod listen_table;
/// Socket option types and the [`Configurable`](options::Configurable) trait.
pub mod options;
mod orphan;
mod poll_runtime;
mod queue_runtime;
/// Raw socket implementation.
pub mod raw;
mod router;
mod rx_meta;
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

use alloc::{
    borrow::ToOwned, boxed::Box, format, string::String, sync::Arc, task::Wake, vec, vec::Vec,
};
use core::{
    net::{IpAddr, Ipv4Addr},
    sync::atomic::{AtomicBool, AtomicU8, Ordering},
    time::Duration,
};

use ax_lazyinit::{LazyLock, OnceLock};
use ax_sync::Mutex;
use axpoll::{IoEvents, PollSet};
pub use error::{NetError, NetResult};
use rand_chacha::ChaCha20Rng;
use rand_core::{RngCore, SeedableRng};
pub use rd_net::{WifiLinkPolicy, WifiOperation, WifiTransaction, Wpa2Pmk};
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
    poll_runtime::ProtocolPollRuntime,
    router::{RouteTable, Router, Rule, SharedRouteTable},
    service::{NetControl, NetInterface, Service},
    wrapper::SocketSetWrapper,
};
pub use self::{
    config::{
        DeviceBinding, InterfaceConfig, InterfaceFlags, InterfaceId, InterfaceInfo, InterfaceKind,
        InterfaceMatcher, Ipv4InterfaceConfig, NetworkConfig, RouteInfo, StaticIpConfig,
    },
    device::{ArpEntry, EthernetFramePort, EthernetFramePortList, NetDeviceError, NetDeviceResult},
    queue_runtime::{
        NetQueueStats, NetworkDeviceInput, NetworkQueueRuntime, NetworkRuntimeBuilder,
        NetworkRuntimeError, PinnedNetIrqAction, PinnedNetIrqError, PinnedNetIrqOutcome,
        PinnedNetIrqRegistrar, PinnedNetIrqRegistration, ResolvedNetIrqSource, TxQueueDiscipline,
    },
    router::NetDevStats,
    socket::{
        CMsgData, IpCmsg, RecvFlags, RecvOptions, SendFlags, SendOptions, Shutdown, Socket,
        SocketAddrEx, SocketCmsg, SocketOps,
    },
};

static LISTEN_TABLE: LazyLock<ListenTable> = LazyLock::new(ListenTable::new);
static SOCKET_SET: LazyLock<SocketSetWrapper> = LazyLock::new(SocketSetWrapper::new);

static SERVICE: OnceLock<Mutex<Service>> = OnceLock::new();
static NET_CONTROL: OnceLock<Arc<NetControl>> = OnceLock::new();
static QUEUE_RUNTIME: OnceLock<Mutex<NetworkQueueRuntime>> = OnceLock::new();
static WIFI_INTERFACES: OnceLock<Vec<WifiInterfaceControl>> = OnceLock::new();
static WIFI_ENTROPY: OnceLock<Mutex<WifiEntropy>> = OnceLock::new();
static PROTOCOL_POLL: ProtocolPollRuntime = ProtocolPollRuntime::new();
static PROTOCOL_AFFINITY_STATUS: AtomicU8 = AtomicU8::new(0);
type DeferredPollEntry = (Arc<PollSet>, IoEvents);
static DEFERRED_POLL_WAKE_PENDING: AtomicBool = AtomicBool::new(false);
static DEFERRED_POLL_WAKES: LazyLock<Mutex<Vec<DeferredPollEntry>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

struct WifiInterfaceControl {
    ifname: alloc::string::String,
    device_index: usize,
    mac: EthernetAddress,
    handle: queue_runtime::WifiRuntimeHandle,
}

struct WifiEntropy {
    generator: ChaCha20Rng,
}

impl WifiEntropy {
    fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            generator: ChaCha20Rng::from_seed(seed),
        }
    }

    fn next_connection_entropy(&mut self) -> [u8; 32] {
        let mut entropy = [0; 32];
        self.generator.fill_bytes(&mut entropy);
        entropy
    }
}

fn next_wifi_connection_entropy() -> NetResult<[u8; 32]> {
    if WIFI_ENTROPY.get().is_none() {
        let seed = ax_hal::boot::boot_entropy().ok_or(NetError::EntropyUnavailable)?;
        WIFI_ENTROPY.call_once(|| Mutex::new(WifiEntropy::from_seed(seed)));
    }
    Ok(WIFI_ENTROPY
        .get()
        .expect("Wi-Fi entropy was initialized above")
        .lock()
        .next_connection_entropy())
}

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
        // defer the actual PollSet wake to the protocol executor outer loop.
        defer_poll_wake(self.poll.clone(), self.ready);
    }
}

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

fn map_driver_net_error(error: rd_net::NetError) -> NetError {
    match error {
        rd_net::NetError::NotSupported | rd_net::NetError::IrqUnavailable => {
            NetError::OperationNotSupported
        }
        rd_net::NetError::Retry => NetError::ResourceBusy,
        rd_net::NetError::NoMemory => NetError::NoMemory,
        rd_net::NetError::LinkDown => NetError::NoSuchDeviceOrAddress,
        rd_net::NetError::InvalidParts => NetError::InvalidData,
        rd_net::NetError::Stopped | rd_net::NetError::DmaShutdownUnconfirmed => NetError::BadState,
        rd_net::NetError::Other(_) => NetError::BackendIo,
    }
}

/// Atomically reconfigures one wireless interface through its fixed-CPU queue
/// owner, then commits the matching protocol-side IP/DHCP role.
///
/// The calling task only submits a bounded command and waits for completion. It
/// never gains access to the wireless control endpoint or SDIO/MMIO state.
pub fn reconfigure_wifi(ifname: &str, mut transaction: WifiTransaction) -> NetResult {
    let interface = WIFI_INTERFACES
        .get()
        .and_then(|interfaces| {
            interfaces
                .iter()
                .find(|interface| interface.ifname == ifname)
        })
        .ok_or(NetError::NoSuchDevice)?;

    if transaction.needs_connect_entropy() {
        transaction.provide_connect_entropy(next_wifi_connection_entropy()?);
        log::info!("[wifi] {ifname}: secure connection entropy prepared");
    }

    log::info!("[wifi] {ifname}: submitting control transaction");
    if let Err(error) = interface.handle.submit(transaction.clone()) {
        log::error!("[wifi] {ifname}: control transaction failed: {error:?}");
        return Err(map_driver_net_error(error));
    }
    log::info!("[wifi] {ifname}: control transaction complete");

    let mut service = get_service();
    match transaction.operation() {
        WifiOperation::Connect { .. } => {
            service.reconfigure_as_sta(interface.device_index, interface.mac);
        }
        WifiOperation::Disconnect => {
            service.reconfigure_as_disconnected(interface.device_index);
        }
        WifiOperation::StartOpenAccessPoint { .. } => {
            let policy = transaction.link_policy().ok_or(NetError::InvalidInput)?;
            service.reconfigure_as_ap(
                interface.device_index,
                Ipv4Address::from(policy.ip),
                policy.prefix_len,
                policy.dhcp_server_client_ip.map(Ipv4Address::from),
            );
        }
    }
    drop(service);
    request_poll();
    Ok(())
}

#[cfg(test)]
mod wifi_entropy_tests {
    use alloc::boxed::Box;

    use super::{NetError, WifiEntropy, default_interface_name, map_driver_net_error};

    #[test]
    fn one_seed_produces_unique_entropy_for_each_connection() {
        let mut source = WifiEntropy::from_seed([0x5a; 32]);
        let first = source.next_connection_entropy();
        let second = source.next_connection_entropy();
        assert_ne!(first, second);
        assert_ne!(first, [0; 32]);
        assert_ne!(second, [0; 32]);
    }

    #[test]
    fn wifi_capability_preserves_the_driver_registered_interface_name() {
        assert_eq!(default_interface_name(0, "wlan0", true), "wlan0");
        assert_eq!(default_interface_name(0, "virtio-net", false), "eth0");
    }

    #[test]
    fn driver_io_failures_do_not_become_bad_user_addresses() {
        let driver_error = rd_net::NetError::Other(Box::new(ax_io::IoError::Io));
        assert_eq!(map_driver_net_error(driver_error), NetError::BackendIo);
    }
}

/// Initializes the network subsystem by NIC devices.
///
/// # Panics
///
/// Panics if called more than once, or if the configuration contains invalid values.
pub fn init_network(
    queue_runtime: Option<NetworkQueueRuntime>,
    mut frame_ports: EthernetFramePortList,
    config: NetworkConfig,
) {
    if SERVICE.get().is_some() {
        panic!("init_network() called more than once");
    }

    info!("Initialize network subsystem...");

    validate_config(&config);

    let routes: SharedRouteTable = Arc::new(ax_sync::SpinRwLock::new(RouteTable::new()));
    let mut router = Router::new(routes.clone());
    let mut interfaces = Vec::new();
    let mut dns = Vec::new();

    let lo_ip = register_loopback(&mut router, &mut interfaces);

    if frame_ports.is_empty() {
        warn!("  No network device found!");
    }

    let mut used_configs = vec![false; config.interfaces.len()];
    let mut dhcp_ifaces = Vec::new();
    let mut eth_ips = Vec::new();
    let mut wifi_dhcp_servers = Vec::new();
    let mut wifi_interfaces = Vec::new();

    for (order, dev) in frame_ports.drain(..).enumerate() {
        info!("  use NIC {}: {:?}", order, dev.device_name());
        let wifi_capable = queue_runtime
            .as_ref()
            .and_then(|runtime| runtime.wifi_handle(order))
            .is_some();
        let default_name = default_interface_name(order, dev.device_name(), wifi_capable);
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
        let wifi_policy = queue_runtime
            .as_ref()
            .and_then(|runtime| runtime.initial_wifi_policy(order));
        let static_ip = cfg.and_then(|cfg| cfg.static_ip.as_ref());
        let ipv4 = static_ip
            .map(|cfg| Ipv4Cidr::new(Ipv4Address::from(cfg.ip.octets()), cfg.prefix_len))
            .or_else(|| {
                (cfg.is_none())
                    .then_some(wifi_policy)
                    .flatten()
                    .map(|policy| Ipv4Cidr::new(Ipv4Address::from(policy.ip), policy.prefix_len))
            });
        let gateway = static_ip.and_then(|cfg| {
            (!cfg.gateway.is_unspecified()).then(|| Ipv4Address::from(cfg.gateway.octets()))
        });
        let dhcp_enabled = cfg.map_or(wifi_policy.is_none(), |cfg| cfg.dhcp);
        let eth_dev = router.add_device(id, Box::new(EthernetDevice::new(name.clone(), dev, ipv4)));

        if let Some(handle) = queue_runtime
            .as_ref()
            .and_then(|runtime| runtime.wifi_handle(order))
        {
            info!(
                "  Wi-Fi control for {name} is owned by CPU {}",
                handle.owner_cpu()
            );
            wifi_interfaces.push(WifiInterfaceControl {
                ifname: name.clone(),
                device_index: order,
                mac,
                handle,
            });
        }

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
        if cfg.is_none()
            && let Some(policy) = wifi_policy
            && let Some(client_ip) = policy.dhcp_server_client_ip
        {
            wifi_dhcp_servers.push((
                order,
                Ipv4Address::from(policy.ip),
                Ipv4Address::from(client_ip),
                mask_from_prefix(policy.prefix_len),
            ));
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
    for (dev, server_ip, client_ip, subnet_mask) in wifi_dhcp_servers {
        service.enable_dhcp_server(dev, server_ip, client_ip, subnet_mask);
    }
    let dhcp_enabled = service.dhcp_enabled();
    let protocol_owner_cpu = queue_runtime.as_ref().map_or_else(
        ax_hal::percpu::this_cpu_id,
        NetworkQueueRuntime::protocol_owner_cpu,
    );
    NET_CONTROL.call_once(|| control);
    SERVICE.call_once(|| Mutex::new(service));
    WIFI_INTERFACES.call_once(|| wifi_interfaces);
    if let Some(runtime) = queue_runtime {
        QUEUE_RUNTIME.call_once(|| Mutex::new(runtime));
    }
    start_protocol_executor(protocol_owner_cpu);
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

fn default_interface_name(order: usize, driver_name: &str, wifi_capable: bool) -> String {
    if wifi_capable {
        driver_name.into()
    } else {
        format!("eth{order}")
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

fn poll_protocol_until_idle() {
    loop {
        if !get_service().poll(&mut SOCKET_SET.inner.lock()) {
            return;
        }
    }
}

/// Request network polling.
///
/// This is the lightweight entry used by socket and device paths.
pub fn request_poll() {
    let _ = PROTOCOL_POLL.request();
}

/// Waits for the unique protocol executor to dispatch all work published by
/// this caller.
///
/// [`request_poll`] only wakes the protocol executor; the actual dispatch happens
/// later. A socket that is closed in the same breath as its last send would
/// otherwise be torn down before the executor runs, discarding the datagram still
/// queued in its TX buffer. Draining egress here mirrors Linux, where a sent
/// datagram already sits in the peer's receive buffer and `close()` cannot
/// unsend it. Must not be called while holding `SOCKET_SET.inner`.
pub(crate) fn flush_egress() {
    let generation = PROTOCOL_POLL.request();
    #[cfg(test)]
    {
        // Host unit tests install protocol state without starting an ArceOS
        // scheduler.  Completing the generation exercises the wait contract
        // without letting the caller execute smoltcp as a second owner.
        PROTOCOL_POLL.complete(generation);
    }
    #[cfg(not(test))]
    PROTOCOL_POLL.wait_for_completion(generation);
}

pub(crate) fn defer_poll_wake(poll: Arc<PollSet>, ready: IoEvents) {
    DEFERRED_POLL_WAKES.lock().push((poll, ready));
    if !DEFERRED_POLL_WAKE_PENDING.swap(true, Ordering::AcqRel) {
        PROTOCOL_POLL.schedule();
    }
}

fn drain_deferred_poll_wakes() {
    loop {
        let wakes = {
            let mut wakes = DEFERRED_POLL_WAKES.lock();
            if wakes.is_empty() {
                DEFERRED_POLL_WAKE_PENDING.store(false, Ordering::Release);
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
pub fn set_interface_ipv4(interface_id: InterfaceId, ip: Ipv4Addr, prefix_len: u8) -> NetResult {
    {
        let mut service = get_service();
        service.configure_static_ipv4(interface_id, Ipv4Address::from(ip.octets()), prefix_len)?;
    }
    request_poll();
    Ok(())
}

/// Removes a configured IPv4 address from an interface at runtime.
pub fn remove_interface_ipv4(interface_id: InterfaceId, ip: Ipv4Addr, prefix_len: u8) -> NetResult {
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

fn next_poll_delay() -> Option<Duration> {
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

fn start_protocol_executor(owner_cpu: usize) {
    PROTOCOL_AFFINITY_STATUS.store(0, Ordering::Release);
    ax_task::spawn_with_name(
        move || {
            let affinity = ax_task::AxCpuMask::one_shot(owner_cpu);
            if !ax_task::set_current_affinity(affinity) {
                PROTOCOL_AFFINITY_STATUS.store(2, Ordering::Release);
                return;
            }
            ax_task::yield_now();
            if ax_hal::percpu::this_cpu_id() != owner_cpu {
                PROTOCOL_AFFINITY_STATUS.store(2, Ordering::Release);
                return;
            }
            PROTOCOL_AFFINITY_STATUS.store(1, Ordering::Release);
            PROTOCOL_POLL.schedule();
            protocol_executor_main();
        },
        "net-protocol".to_owned(),
    );
    while PROTOCOL_AFFINITY_STATUS.load(Ordering::Acquire) == 0 {
        ax_task::yield_now();
    }
    assert_eq!(
        PROTOCOL_AFFINITY_STATUS.load(Ordering::Acquire),
        1,
        "failed to pin the unique network protocol executor to CPU {owner_cpu}"
    );
}

fn protocol_executor_main() {
    loop {
        if let Some(delay) = next_poll_delay() {
            let _ = PROTOCOL_POLL.wait_timeout(delay);
        } else {
            PROTOCOL_POLL.wait();
        }
        drain_deferred_poll_wakes();
        let completed = PROTOCOL_POLL.requested_generation();
        poll_protocol_until_idle();
        PROTOCOL_POLL.complete(completed);
        drain_deferred_poll_wakes();
        if PROTOCOL_POLL.finish_cycle(|| DEFERRED_POLL_WAKE_PENDING.load(Ordering::Acquire)) {
            continue;
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
pub fn dns_query(name: &str) -> NetResult<Vec<IpAddr>> {
    dns_query_timeout(name, DNS_DEFAULT_TIMEOUT)
}

/// Resolves an A record using the configured DNS servers and timeout.
pub fn dns_query_timeout(name: &str, timeout: Duration) -> NetResult<Vec<IpAddr>> {
    let servers = dns_servers();
    if servers.is_empty() {
        return Err(NetError::NotFound);
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
        return Err(NetError::NoSuchDeviceOrAddress);
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
    ) -> NetResult<Vec<IpAddr>> {
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
            StartQueryError::NoFreeSlot => NetError::ResourceBusy,
            StartQueryError::InvalidName => NetError::InvalidInput,
            StartQueryError::NameTooLong => NetError::InvalidInput,
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
                        GetQueryResultError::Pending => NetError::WouldBlock,
                        GetQueryResultError::Failed => NetError::ConnectionRefused,
                    })
            }) {
                Ok(addrs) => {
                    return Ok(addrs.into_iter().map(IpAddr::from).collect());
                }
                Err(NetError::WouldBlock) => {
                    if ax_hal::time::monotonic_time_nanos() >= deadline {
                        return Err(NetError::TimedOut);
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
            let routes: SharedRouteTable = Arc::new(ax_sync::SpinRwLock::new(RouteTable::new()));
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
