//! [ArceOS](https://github.com/rcore-os/arceos) network module.
//!
//! It provides unified networking primitives for TCP/UDP communication
//! using various underlying network stacks. Currently, only [smoltcp] is
//! supported.
//!
//! # Organization
//!
//! - [`tcp::TcpSocket`]: A TCP socket that provides POSIX-like APIs.
//! - [`udp::UdpSocket`]: A UDP socket that provides POSIX-like APIs.
//!
//! [smoltcp]: https://github.com/smoltcp-rs/smoltcp

#![no_std]

#[macro_use]
extern crate log;
extern crate alloc;
#[cfg(test)]
extern crate std;

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

use alloc::{borrow::ToOwned, boxed::Box, vec::Vec};
use core::{
    future::poll_fn,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
    time::Duration,
};

use ax_sync::Mutex;
use ax_task::future::block_on;
use axpoll::PollSet;
use smoltcp::wire::{EthernetAddress, Ipv4Address, Ipv4Cidr};
use spin::{LazyLock, Once};

#[cfg(feature = "vsock")]
pub use self::device::{VsockDevice, VsockDeviceList};
use self::{
    consts::{GATEWAY, IP, IP_PREFIX},
    device::{EthernetDevice, LoopbackDevice},
    listen_table::ListenTable,
    router::{Router, Rule},
    service::Service,
    wrapper::SocketSetWrapper,
};
pub use self::{
    device::{
        ArpEntry, EthernetDeviceList, EthernetDriver, NetDeviceError, NetDeviceResult,
        NetIrqEvents, NetRxBuffer, NetTxBuffer, RdNetDriver,
    },
    socket::*,
};

static LISTEN_TABLE: LazyLock<ListenTable> = LazyLock::new(ListenTable::new);
static SOCKET_SET: LazyLock<SocketSetWrapper> = LazyLock::new(SocketSetWrapper::new);

static SERVICE: Once<Mutex<Service>> = Once::new();
static POLLING_INTERFACES: AtomicBool = AtomicBool::new(false);
static POLL_AGAIN: AtomicBool = AtomicBool::new(false);

/// Signalled by [`notify_wifi_rx`] (AIC8800 RX thread) to wake `wlan0-poll`.
static WIFI_RX_SIGNAL: PollSet = PollSet::new();
static WIFI_POLL_TASK_STARTED: AtomicBool = AtomicBool::new(false);

const DHCP_BOOTSTRAP_ATTEMPTS: usize = 200;
const DHCP_BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn get_service() -> ax_sync::MutexGuard<'static, Service> {
    SERVICE
        .get()
        .expect("Network service not initialized")
        .lock()
}

/// Initializes the network subsystem by NIC devices.
pub fn init_network(mut net_devs: EthernetDeviceList) {
    info!("Initialize network subsystem...");

    let mut router = Router::new();
    let lo_dev = router.add_device(Box::new(LoopbackDevice::new()));

    let lo_ip = Ipv4Cidr::new(Ipv4Address::new(127, 0, 0, 1), 8);
    router.add_rule(Rule::new(
        lo_ip.into(),
        None,
        lo_dev,
        lo_ip.address().into(),
    ));

    let static_network = !IP.is_empty() && !GATEWAY.is_empty();
    let mut dhcp_dev = None;
    let mut dhcp_mac = None;

    let eth0_ip = if !net_devs.is_empty() {
        let dev = net_devs.remove(0);
        info!("  use NIC 0: {:?}", dev.device_name());

        let eth0_address = EthernetAddress(dev.mac_address());
        let eth0_ip = static_network
            .then(|| Ipv4Cidr::new(IP.parse().expect("Invalid IPv4 address"), IP_PREFIX));

        let eth0_dev = router.add_device(Box::new(EthernetDevice::new(
            "eth0".to_owned(),
            dev,
            eth0_ip,
        )));

        info!("eth0:");
        info!("  mac:  {}", eth0_address);
        if let Some(eth0_ip) = eth0_ip {
            let gateway = GATEWAY.parse().expect("Invalid gateway address");
            router.add_rule(Rule::new(
                Ipv4Cidr::new(Ipv4Address::UNSPECIFIED, 0).into(),
                Some(gateway),
                eth0_dev,
                eth0_ip.address().into(),
            ));
            info!("  mode: static");
            info!("  ip:   {}", eth0_ip);
            info!("  gw:   {}", gateway);
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

    let mut service = Service::new(router);
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

/// Registers the AIC8800 Wi-Fi device as a SoftAP `wlan0` with a static IPv4
/// and a built-in single-client DHCP server.
///
/// `dev` is the already-wrapped ethernet driver (an `RdNetDriver` around the
/// AIC8800 `Interface`). `server_ip` becomes wlan0's address and gateway;
/// `client_ip` is handed out over DHCP. The network service must already be
/// initialized (via [`init_network`]).
pub fn register_wifi_ap_device(
    dev: Box<dyn EthernetDriver>,
    server_ip: [u8; 4],
    client_ip: [u8; 4],
    prefix_len: u8,
) {
    let server_ip = Ipv4Address::new(server_ip[0], server_ip[1], server_ip[2], server_ip[3]);
    let client_ip = Ipv4Address::new(client_ip[0], client_ip[1], client_ip[2], client_ip[3]);
    let cidr = Ipv4Cidr::new(server_ip, prefix_len);
    let subnet_mask = prefix_to_mask(prefix_len);

    let mac = EthernetAddress(dev.mac_address());
    let eth_dev = EthernetDevice::new("wlan0".to_owned(), dev, Some(cidr));

    {
        let mut s = get_service();
        let dev_idx = s.register_static_device("wlan0".to_owned(), eth_dev, cidr);
        s.iface.update_ip_addrs(|ip_addrs| {
            let addr = smoltcp::wire::IpCidr::Ipv4(cidr);
            if !ip_addrs.contains(&addr) {
                ip_addrs.push(addr).ok();
            }
        });
        s.enable_dhcp_server(dev_idx, server_ip, client_ip, subnet_mask);
    }

    info!("wlan0: SoftAP up, mac {mac}, ip {cidr}");
    start_wifi_poll_task();
}

/// Wakes the `wlan0` poll task; registered as the AIC8800 RX-data callback.
///
/// The AIC8800 RX thread owns the SDIO CARD_INT and is outside the ethernet
/// IRQ framework, so it only signals here; the `wlan0-poll` task does the
/// actual stack polling to avoid starving SDIO RX.
pub fn notify_wifi_rx() {
    WIFI_RX_SIGNAL.wake();
}

/// Spawns the `wlan0-poll` task (idempotent).
pub fn start_wifi_poll_task() {
    if WIFI_POLL_TASK_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    ax_task::spawn_with_name(
        || {
            block_on(poll_fn(|cx| {
                // Register first to avoid lost wakeups.
                WIFI_RX_SIGNAL.register(cx.waker());
                poll_interfaces();
                get_service().wake_all_devices();
                Poll::<()>::Pending
            }));
        },
        "wlan0-poll".to_owned(),
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

            let mut service = Service::new(router);
            service.iface.update_ip_addrs(|ip_addrs| {
                ip_addrs.push(local_cidr.into()).unwrap();
                ip_addrs.push(peer_cidr.into()).unwrap();
            });

            SERVICE.call_once(|| Mutex::new(service));
        });
    }
}
