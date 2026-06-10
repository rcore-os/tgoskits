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
    net::IpAddr,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult, ax_err_type};
use ax_sync::Mutex;
use smoltcp::{
    socket::dns::{self, GetQueryResultError, StartQueryError},
    wire::{DnsQueryType, EthernetAddress, IpAddress, Ipv4Address, Ipv4Cidr},
};
use spin::{LazyLock, Once};

#[cfg(feature = "vsock")]
pub use self::device::{VsockDevice, VsockDeviceList};
pub use self::{
    config::{NetworkConfig, StaticIpConfig},
    device::{
        ArpEntry, EthernetDeviceList, EthernetDriver, NetDeviceError, NetDeviceResult,
        NetIrqEvents, NetRxBuffer, NetTxBuffer, RdNetDriver,
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
    use alloc::boxed::Box;
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
