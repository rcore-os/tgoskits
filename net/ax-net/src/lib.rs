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
mod dhcp_server;
mod general;
mod listen_table;
/// Socket option types and the [`Configurable`](options::Configurable) trait.
pub mod options;
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

use alloc::{borrow::ToOwned, boxed::Box, vec, vec::Vec};
use core::{
    future::poll_fn,
    net::IpAddr,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
    time::Duration,
};

use ax_errno::{AxError, AxResult, ax_err_type};
use ax_sync::Mutex;
use ax_task::future::block_on;
use axpoll::PollSet;
use smoltcp::{
    socket::dns::{self, GetQueryResultError, StartQueryError},
    wire::{DnsQueryType, EthernetAddress, IpAddress, Ipv4Address, Ipv4Cidr},
};
use spin::{LazyLock, Once};

#[cfg(feature = "vsock")]
pub use self::device::{VsockDevice, VsockDeviceList};
pub use self::{
    config::{Ipv4InterfaceConfig, NetworkConfig, StaticIpConfig},
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
    router::{Router, Rule},
    service::Service,
    wrapper::SocketSetWrapper,
};

static LISTEN_TABLE: LazyLock<ListenTable> = LazyLock::new(ListenTable::new);
static SOCKET_SET: LazyLock<SocketSetWrapper> = LazyLock::new(SocketSetWrapper::new);

static SERVICE: Once<Mutex<Service>> = Once::new();
static POLLING_INTERFACES: AtomicBool = AtomicBool::new(false);
static POLL_AGAIN: AtomicBool = AtomicBool::new(false);

/// Registry of wireless control-plane handles, keyed by interface name.
///
/// Populated when a wireless device is registered (the runtime captures a
/// [`rd_net::WifiControlHandle`] before the `Net` is consumed into the data-plane
/// driver). Lets runtime mode switching (e.g. a StarryOS wireless-extensions
/// `ioctl`) reach the device's [`WifiControl`] by name.
static WIFI_CONTROLS: LazyLock<Mutex<Vec<(alloc::string::String, rd_net::WifiControlHandle)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Signalled by [`notify_oob_rx`] to wake the out-of-band RX poll task, for
/// devices whose RX arrives outside the ethernet IRQ framework (e.g. SDIO).
static OOB_RX_SIGNAL: PollSet = PollSet::new();
static OOB_POLL_TASK_STARTED: AtomicBool = AtomicBool::new(false);

const DHCP_BOOTSTRAP_ATTEMPTS: usize = 200;
const DHCP_BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn get_service() -> ax_sync::MutexGuard<'static, Service> {
    SERVICE
        .get()
        .expect("Network service not initialized")
        .lock()
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

    // Validate configuration
    if let Some(ref static_cfg) = config.static_ip {
        if static_cfg.ip.is_unspecified() {
            panic!("Invalid static IP: unspecified address");
        }
        if static_cfg.prefix_len > 32 {
            panic!("Invalid static IP: prefix length > 32");
        }
        if static_cfg.gateway.is_unspecified() {
            panic!("Invalid gateway: unspecified address");
        }
    }
    for (i, dns) in config.dns_servers.iter().enumerate() {
        if dns.is_unspecified() {
            panic!("Invalid DNS server at index {}: unspecified address", i);
        }
    }

    // Convert DNS servers to smoltcp types
    let static_dns: Vec<Ipv4Address> = config
        .dns_servers
        .iter()
        .map(|addr| Ipv4Address::from(addr.octets()))
        .collect();

    let mut router = Router::new();
    let lo_dev = router.add_device(Box::new(LoopbackDevice::new()));

    let lo_ip = Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8);
    router.add_rule(Rule::new(
        lo_ip.into(),
        None,
        lo_dev,
        lo_ip.address().into(),
    ));

    let mut dhcp_dev = None;
    let mut dhcp_mac = None;

    let eth0_ip = if !net_devs.is_empty() {
        let dev = net_devs.remove(0);
        info!("  use NIC 0: {:?}", dev.device_name());

        let eth0_address = EthernetAddress(dev.mac_address());
        let eth0_ip = config
            .static_ip
            .as_ref()
            .map(|cfg| Ipv4Cidr::new(Ipv4Address::from(cfg.ip.octets()), cfg.prefix_len));

        let eth0_dev = router.add_device(Box::new(EthernetDevice::new(
            "eth0".to_owned(),
            dev,
            eth0_ip,
        )));

        info!("eth0:");
        info!("  mac:  {}", eth0_address);
        if let Some(static_cfg) = &config.static_ip {
            router.add_rule(Rule::new(
                Ipv4Cidr::new(Ipv4Address::UNSPECIFIED, 0).into(),
                Some(Ipv4Address::from(static_cfg.gateway.octets()).into()),
                eth0_dev,
                Ipv4Address::from(static_cfg.ip.octets()).into(),
            ));
            info!("  mode: static");
            info!("  ip:   {}/{}", static_cfg.ip, static_cfg.prefix_len);
            info!("  gw:   {}", static_cfg.gateway);
        } else {
            dhcp_dev = Some(eth0_dev);
            dhcp_mac = Some(eth0_address);
            info!("  mode: dhcp");
        }

        eth0_ip
    } else {
        warn!("  No network device found!");
        None
    };

    for dev in &router.devices {
        info!("Device: {}", dev.name());
    }

    let mut service = Service::new(router, static_dns);
    service.iface.update_ip_addrs(|ip_addrs| {
        ip_addrs.push(lo_ip.into()).unwrap();
        if let Some(eth0_ip) = eth0_ip {
            ip_addrs.push(eth0_ip.into()).unwrap();
        }
    });
    if let (Some(dhcp_dev), Some(dhcp_mac)) = (dhcp_dev, dhcp_mac) {
        service.enable_dhcp(dhcp_dev, dhcp_mac);
    }
    let dhcp_enabled = service.dhcp_enabled();
    SERVICE.call_once(|| Mutex::new(service));
    if dhcp_enabled {
        ax_task::spawn_with_name(dhcp_bootstrap, "dhcp-bootstrap".to_owned());
    }
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

/// Poll all network interfaces for new events.
pub fn poll_interfaces() {
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

pub fn arp_entries() -> Vec<ArpEntry> {
    get_service().arp_entries()
}

/// Stack-agnostic configuration for registering an already-wrapped ethernet
/// device with a static IPv4 and optional services.
///
/// This carries no notion of "Wi-Fi" or "SoftAP" — it is the generic policy the
/// protocol stack applies. Link-type-specific policy (e.g. a SoftAP's choice of
/// addresses and DHCP-server lease) is decided by the caller (board/runtime) and
/// passed in as data.
pub struct NetConfig {
    /// Interface name (e.g. `"wlan0"`).
    pub name: alloc::string::String,
    /// This interface's static address / gateway.
    pub ip: [u8; 4],
    pub prefix_len: u8,
    /// If set, run a built-in DHCP server handing out this single address.
    pub dhcp_server_client_ip: Option<[u8; 4]>,
    /// Spawn a dedicated poll task woken via [`notify_oob_rx`]. Needed for
    /// out-of-band RX devices (e.g. SDIO) that sit outside the ethernet IRQ
    /// framework.
    pub dedicated_poll: bool,
}

/// Registers an already-wrapped ethernet device with a static IPv4 and the
/// services described by `config`. The network service must already be
/// initialized (via [`init_network`]).
///
/// This is the generic, link-type-agnostic registration entry point. A SoftAP
/// is just one caller that fills in a static IP + DHCP server + dedicated poll.
pub fn register_device_with_config(dev: Box<dyn EthernetDriver>, config: NetConfig) {
    let server_ip = Ipv4Address::new(config.ip[0], config.ip[1], config.ip[2], config.ip[3]);
    let cidr = Ipv4Cidr::new(server_ip, config.prefix_len);

    let mac = EthernetAddress(dev.mac_address());
    // A dedicated-poll device gets RX out-of-band (via `notify_oob_rx` →
    // `wake_rx`), so its socket wakers must be armed even though it has no
    // ethernet IRQ registration.
    let eth_dev = if config.dedicated_poll {
        EthernetDevice::new_oob_rx(config.name.clone(), dev, Some(cidr))
    } else {
        EthernetDevice::new(config.name.clone(), dev, Some(cidr))
    };

    {
        let mut s = get_service();
        let dev_idx = s.register_static_device(config.name.clone(), eth_dev, cidr);
        if let Some(client) = config.dhcp_server_client_ip {
            let client_ip = Ipv4Address::new(client[0], client[1], client[2], client[3]);
            let subnet_mask = prefix_to_mask(config.prefix_len);
            s.enable_dhcp_server(dev_idx, server_ip, client_ip, subnet_mask);
        }
    }

    info!("{}: up, mac {mac}, ip {cidr}", config.name);
    if config.dedicated_poll {
        start_oob_poll_task(config.name);
    }
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
        let controls = WIFI_CONTROLS.lock();
        let (_, handle) = controls
            .iter()
            .find(|(n, _)| n == name)
            .ok_or(AxError::NoSuchDevice)?;
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
                let server_ip = Ipv4Address::new(ip[0], ip[1], ip[2], ip[3]);
                let client_ip = dhcp_client_ip.map(|c| Ipv4Address::new(c[0], c[1], c[2], c[3]));
                service.reconfigure_as_ap(dev, server_ip, prefix_len, client_ip);
            }
        }
    }

    // Kick a poll so the new addressing takes effect immediately.
    poll_interfaces();
    info!("{name}: wifi mode switch complete");
    Ok(())
}

/// Wakes the out-of-band RX poll task; intended as a device RX-data callback.
///
/// A device whose RX path sits outside the ethernet IRQ framework (e.g. an SDIO
/// chip owning its own card interrupt) registers this as its RX callback. It
/// only signals here; the dedicated poll task does the actual stack polling, so
/// the device's RX thread is never blocked on the stack.
pub fn notify_oob_rx() {
    OOB_RX_SIGNAL.wake();
}

/// Spawns the out-of-band RX poll task (idempotent across all such devices).
///
/// `ifname` names the task (e.g. `wlan0` → `wlan0-poll`). One shared task drives
/// `poll_interfaces()` for every dedicated-poll device, woken by [`notify_oob_rx`].
fn start_oob_poll_task(ifname: alloc::string::String) {
    if OOB_POLL_TASK_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    ax_task::spawn_with_name(
        || {
            block_on(poll_fn(|cx| {
                // Register first to avoid lost wakeups.
                OOB_RX_SIGNAL.register(cx.waker());
                poll_interfaces();
                get_service().wake_all_devices();
                Poll::<()>::Pending
            }));
        },
        alloc::format!("{ifname}-poll"),
    );
}

fn prefix_to_mask(prefix_len: u8) -> Ipv4Address {
    let bits: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len.min(32) as u32)
    };
    Ipv4Address::from_bits(bits)
}

pub fn eth0_ipv4_config() -> Option<Ipv4InterfaceConfig> {
    get_service().eth0_ipv4_config()
}

/// Returns the list of configured DNS servers.
///
/// Priority: DHCP-provided servers take precedence over statically configured servers.
/// If DHCP hasn't provided servers, falls back to the servers from `NetworkConfig`.
pub fn dns_servers() -> Vec<Ipv4Address> {
    get_service().dns_servers()
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

    let servers = servers.into_iter().map(IpAddress::Ipv4).collect::<Vec<_>>();
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
            poll_interfaces();
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

fn dhcp_bootstrap() {
    for _ in 0..DHCP_BOOTSTRAP_ATTEMPTS {
        poll_interfaces();
        if get_service().dhcp_configured() {
            return;
        }
        ax_task::sleep(DHCP_BOOTSTRAP_POLL_INTERVAL);
    }
    warn!("eth0: DHCP bootstrap timed out");
}

#[cfg(test)]
pub(crate) mod test_support {
    use alloc::{boxed::Box, vec::Vec};
    use std::sync::{Mutex as StdMutex, MutexGuard, Once};

    use ax_sync::Mutex;
    use smoltcp::wire::{IpAddress, Ipv4Address, Ipv4Cidr};

    use crate::{
        SERVICE,
        device::LoopbackDevice,
        router::{Router, Rule},
        service::Service,
    };

    pub(crate) const LOCAL_MASK: u32 = 1 << 0;
    pub(crate) const PEER_MASK: u32 = 1 << 1;
    pub(crate) const LOCAL_ADDR: Ipv4Address = Ipv4Address::new(192, 0, 2, 10);
    pub(crate) const PEER_ADDR: Ipv4Address = Ipv4Address::new(198, 51, 100, 20);

    static NETWORK_TEST_LOCK: StdMutex<()> = StdMutex::new(());

    pub(crate) fn network_test_guard() -> MutexGuard<'static, ()> {
        NETWORK_TEST_LOCK.lock().unwrap()
    }

    pub(crate) fn init_split_route_network() {
        static INIT: Once = Once::new();

        INIT.call_once(|| {
            let mut router = Router::new();
            let local_dev = router.add_device(Box::new(LoopbackDevice::new()));
            let peer_dev = router.add_device(Box::new(LoopbackDevice::new()));
            let local_cidr = Ipv4Cidr::new(LOCAL_ADDR, 24);
            let peer_cidr = Ipv4Cidr::new(PEER_ADDR, 24);

            router.add_rule(Rule::new(
                local_cidr.into(),
                None,
                local_dev,
                IpAddress::Ipv4(LOCAL_ADDR),
            ));
            router.add_rule(Rule::new(
                peer_cidr.into(),
                None,
                peer_dev,
                IpAddress::Ipv4(PEER_ADDR),
            ));

            let mut service = Service::new(router, Vec::new());
            service.iface.update_ip_addrs(|ip_addrs| {
                ip_addrs.push(local_cidr.into()).unwrap();
                ip_addrs.push(peer_cidr.into()).unwrap();
            });

            SERVICE.call_once(|| Mutex::new(service));
        });
    }
}
