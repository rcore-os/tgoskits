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
    net::IpAddr,
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
    time::Duration,
};

use ax_errno::{AxError, AxResult, ax_err_type};
use ax_sync::Mutex;
use ax_task::{IrqNotify, WaitQueue};
use axpoll::{IoEvents, PollSet};
use smoltcp::{
    socket::dns::{self, GetQueryResultError, StartQueryError},
    wire::{DnsQueryType, EthernetAddress, IpAddress, Ipv4Address, Ipv4Cidr},
};
use spin::{LazyLock, Once};

#[cfg(feature = "vsock")]
pub use self::device::{VsockDevice, VsockDeviceList};
use self::{
    addr::mask_from_prefix,
    device::{EthernetDevice, LoopbackDevice},
    listen_table::ListenTable,
    router::{RouteTable, Router, Rule, SharedRouteTable},
    service::{NetControl, NetInterface, Service},
    wrapper::SocketSetWrapper,
};
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
        CMsgData, IpCmsg, RecvFlags, RecvOptions, SendFlags, SendOptions, Shutdown, Socket,
        SocketAddrEx, SocketOps,
    },
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
type DeferredPollEntry = (Arc<PollSet>, IoEvents);
static DEFERRED_POLL_WAKE_PENDING: AtomicBool = AtomicBool::new(false);
static DEFERRED_POLL_WAKES: LazyLock<Mutex<Vec<DeferredPollEntry>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

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

/// Registry of wireless control-plane handles, keyed by interface name.
///
/// Populated when a wireless device is registered (the runtime captures a
/// [`rd_net::WifiControlHandle`] before the `Net` is consumed into the data-plane
/// driver). Lets runtime mode switching (e.g. a StarryOS wireless-extensions
/// `ioctl`) reach the device's [`WifiControl`] by name.
static WIFI_CONTROLS: LazyLock<Mutex<Vec<(alloc::string::String, rd_net::WifiControlHandle)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

static NET_IRQ_NOTIFY: IrqNotify = IrqNotify::new();

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
    POLL_AGAIN.store(true, Ordering::Release);
    loop {
        if POLLING_INTERFACES
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        while POLL_AGAIN.swap(false, Ordering::AcqRel) {
            while get_service().poll(&mut SOCKET_SET.inner.lock()) {}
        }
        POLLING_INTERFACES.store(false, Ordering::Release);
        if !POLL_AGAIN.load(Ordering::Acquire) {
            return;
        }
    }
}

/// Request network polling.
///
/// This is the lightweight entry used by socket and device paths.
pub fn request_poll() {
    if !NET_POLL_REQUESTED.swap(true, Ordering::AcqRel) {
        NET_POLL_WAKE.notify_one(true);
    }
}

pub(crate) fn defer_poll_wake(poll: Arc<PollSet>, ready: IoEvents) {
    DEFERRED_POLL_WAKES.lock().push((poll, ready));
    if !DEFERRED_POLL_WAKE_PENDING.swap(true, Ordering::AcqRel) {
        NET_POLL_WAKE.notify_one(true);
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
    /// Whether this device is woken through the out-of-band poll task.
    pub dedicated_poll: bool,
}

/// Registers an extra Ethernet device with a static IPv4 address.
///
/// If `dedicated_poll` is set, RX readiness is driven by [`wake_net_task_irq`]
/// instead of the shared Ethernet IRQ framework.
pub fn register_device_with_config(dev: Box<dyn EthernetDriver>, config: NetConfig) {
    let mac = EthernetAddress(dev.mac_address());
    let server_ip = Ipv4Address::from(config.ip);
    let cidr = Ipv4Cidr::new(server_ip, config.prefix_len);
    // A dedicated-poll device gets RX out-of-band (via `wake_net_task_irq` and the
    // shared net poll task), so its socket wakers must be armed even though it
    // has no ethernet IRQ registration.
    let eth_dev = if config.dedicated_poll {
        EthernetDevice::new_oob_rx(config.name.clone(), dev, Some(cidr))
    } else {
        EthernetDevice::new(config.name.clone(), dev, Some(cidr))
    };
    let dev_idx = get_service().register_static_device(config.name.clone(), eth_dev, mac, cidr);
    if let Some(client_ip) = config.dhcp_server_client_ip {
        let client_ip = Ipv4Address::from(client_ip);
        let subnet_mask = mask_from_prefix(config.prefix_len);
        get_service().enable_dhcp_server(dev_idx, server_ip, client_ip, subnet_mask);
    }

    info!("{}: up, mac {mac}, ip {cidr}", config.name);
    if config.dedicated_poll {
        get_service().register_device_waker(&NET_POLL_DEVICE_WAKER);
    }
    request_poll();
}

/// Registers a wireless control-plane handle under an interface name.
///
/// Called by the runtime when adapting a wireless net device, *before* the
/// `Net` is consumed into the data-plane driver, so the control plane stays
/// reachable by name for runtime mode switching.
pub fn register_wifi_control(name: &str, handle: rd_net::WifiControlHandle) {
    let mut controls = WIFI_CONTROLS.lock();
    if let Some(entry) = controls.iter_mut().find(|(n, _)| n == name) {
        entry.1 = handle;
    } else {
        controls.push((name.into(), handle));
    }
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
    // 1. Link-layer switch through the device control plane, plus the device's
    //    (possibly new) MAC. The registry lock is released before touching the
    //    stack service to avoid holding two locks across the blocking path.
    let mac = {
        let handle = {
            let controls = WIFI_CONTROLS.lock();
            controls
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, handle)| handle.clone())
                .ok_or(AxError::NoSuchDevice)?
        };
        let ctrl = handle.wifi_control().ok_or(AxError::NoSuchDevice)?;
        match &mode {
            WifiMode::Station { ssid, password } => ctrl
                .connect(ssid, password)
                .map_err(|_| ax_err_type!(Unsupported, "wifi STA connect failed"))?,
            WifiMode::AccessPoint { ssid, channel, .. } => ctrl
                .start_ap_open(ssid, *channel)
                .map_err(|_| ax_err_type!(Unsupported, "wifi SoftAP start failed"))?,
        }
        EthernetAddress(handle.mac_address())
    };

    // 2. Reconfigure the stack's IPv4 / DHCP role for this interface.
    {
        let mut service = get_service();
        let dev = service.device_index(name).ok_or(AxError::NoSuchDevice)?;
        match mode {
            WifiMode::Station { .. } => service.reconfigure_as_sta(dev, mac),
            WifiMode::AccessPoint {
                ip,
                prefix_len,
                dhcp_client_ip,
                ..
            } => {
                let server_ip = Ipv4Address::from(ip);
                let client_ip = dhcp_client_ip.map(Ipv4Address::from);
                service.reconfigure_as_ap(dev, server_ip, prefix_len, client_ip);
            }
        }
    }

    // Kick a poll so the new addressing takes effect immediately.
    request_poll();
    info!("{name}: wifi mode switch complete");
    Ok(())
}

/// Wakes the net poll task from a hard IRQ callback.
///
/// The IRQ path must only publish small pending state and call this wrapper.
/// The deferred net task requests polling and wakes socket waiters from ordinary
/// task context.
pub fn wake_net_task_irq() {
    NET_IRQ_NOTIFY.notify_irq();
    NET_POLL_WAKE.notify_one_from_irq();
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
        let timed_out = NET_POLL_WAKE.wait_timeout_until(delay, || {
            NET_POLL_REQUESTED.load(Ordering::Acquire)
                || NET_IRQ_NOTIFY.is_pending()
                || DEFERRED_POLL_WAKE_PENDING.load(Ordering::Acquire)
        });
        if !timed_out && NET_POLL_REQUESTED.load(Ordering::Acquire) {
            NET_POLL_REQUESTED.store(false, Ordering::Release);
        }
        if NET_IRQ_NOTIFY.drain() {
            get_service().wake_all_devices();
        }
        drain_deferred_poll_wakes();
        poll_until_idle();
        drain_deferred_poll_wakes();
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
            SERVICE.call_once(|| Mutex::new(service));
        });
    }
}
