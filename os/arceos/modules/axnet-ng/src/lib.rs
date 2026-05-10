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

use alloc::{borrow::ToOwned, boxed::Box, string::String, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use ax_driver::{AxDeviceContainer, prelude::*};
use ax_hal::time::wall_time_nanos;
use ax_sync::Mutex;
use smoltcp::{
    time::Duration as SmolDuration,
    wire::{EthernetAddress, Ipv4Address, Ipv4Cidr},
};
use spin::{Lazy, Once};

use self::{
    consts::{DHCP_BOOTSTRAP_TIMEOUT, GATEWAY, IP, IP_PREFIX},
    device::{EthernetDevice, LoopbackDevice},
    listen_table::ListenTable,
    router::{Router, Rule},
    service::Service,
    wrapper::SocketSetWrapper,
};
pub use self::{device::ArpEntry, socket::*};

static LISTEN_TABLE: Lazy<ListenTable> = Lazy::new(ListenTable::new);
static SOCKET_SET: Lazy<SocketSetWrapper> = Lazy::new(SocketSetWrapper::new);

static SERVICE: Once<Mutex<Service>> = Once::new();
static POLLING_INTERFACES: AtomicBool = AtomicBool::new(false);
static POLL_AGAIN: AtomicBool = AtomicBool::new(false);

fn get_service() -> ax_sync::MutexGuard<'static, Service> {
    SERVICE
        .get()
        .expect("Network service not initialized")
        .lock()
}

/// Returns DHCP configuration info (IP, gateway, DNS) for display in /proc/net/dhcp.
pub fn dhcp_info() -> Option<String> {
    SERVICE.get()?.lock().dhcp_info()
}

/// Initializes the network subsystem by NIC devices.
pub fn init_network(mut net_devs: AxDeviceContainer<AxNetDevice>) {
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

    let eth0_ip = if let Some(dev) = net_devs.take_one() {
        info!("  use NIC 0: {:?}", dev.device_name());

        let eth0_address = EthernetAddress(dev.mac_address().0);
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
pub fn init_vsock(mut vsock_devs: AxDeviceContainer<AxVsockDevice>) {
    use self::device::register_vsock_device;
    info!("Initialize vsock subsystem...");
    if let Some(dev) = vsock_devs.take_one() {
        info!("  use vsock 0: {:?}", dev.device_name());
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

fn dhcp_bootstrap() {
    let deadline_ns = wall_time_nanos() + DHCP_BOOTSTRAP_TIMEOUT.as_nanos() as u64;
    poll_interfaces();
    while !get_service().dhcp_configured() {
        if wall_time_nanos() >= deadline_ns {
            warn!(
                "eth0: DHCP bootstrap timed out after {:?}",
                DHCP_BOOTSTRAP_TIMEOUT
            );
            return;
        }
        // Poll at least every 100ms so we catch DHCP responses promptly,
        // but also respect the retry timer (dhcp_poll_duration) when it's sooner.
        let poll_dur = get_service()
            .dhcp_poll_duration()
            .unwrap_or(SmolDuration::from_millis(100));
        let sleep = poll_dur.min(SmolDuration::from_millis(100));
        ax_task::sleep(Duration::from_micros(sleep.total_micros().max(1000)));
        poll_interfaces();
    }
    // Bootstrap succeeded — spawn the maintenance task that drives
    // T1/T2/lease-expiry timers for the remainder of the lease.
    ax_task::spawn_with_name(dhcp_maintain, "dhcp-maintain".to_owned());
}

/// Persistently polls the DHCP state machine at the next expected deadline
/// so that T1 (renew), T2 (rebind), and lease expiry are honoured even
/// when no socket I/O would otherwise trigger [`poll_interfaces`].
///
/// [`poll_interfaces`] uses a `POLLING_INTERFACES` CAS re-entrancy guard
/// that bounces concurrent callers.  That is harmless for this task: the
/// guard's `POLL_AGAIN` flag, which we set *before* the CAS, ensures the
/// active poller will do another round — our work is never lost.
fn dhcp_maintain() {
    loop {
        if !get_service().dhcp_enabled() {
            return;
        }
        let poll_dur = match get_service().dhcp_poll_duration() {
            Some(d) => d,
            None => {
                // No DHCP state — should not happen after bootstrap, but
                // sleep a long interval to avoid spinning if it does.
                ax_task::sleep(Duration::from_secs(60));
                poll_interfaces();
                continue;
            }
        };
        let sleep_us = poll_dur.total_micros().max(1000) as u64;
        ax_task::sleep(Duration::from_micros(sleep_us));
        poll_interfaces();
    }
}
