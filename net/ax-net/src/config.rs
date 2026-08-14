//! Network interface and route configuration types.
//!
//! This module is the data model for ax-net's control plane. It is shared by
//! startup configuration, dynamic IPv4 updates, route table replacement, socket
//! device binding, DHCP client/server integration, and userspace interface
//! queries.
//!
//! # Design Notes
//!
//! `InterfaceId` is stable for the lifetime of the stack and is also exported
//! as the Linux ifindex. Route and binding structures refer to this identifier
//! rather than a device vector index so public state survives internal device
//! ordering details.
//!
//! `DeviceBinding` is deliberately small: sockets can bind to an interface, and
//! the service/router layer performs source-address and next-hop selection from
//! the route table. Socket implementations should not duplicate route logic.

use alloc::{string::String, vec::Vec};
use core::net::Ipv4Addr;

use smoltcp::wire::{EthernetAddress, Ipv4Address, Ipv4Cidr};

/// Stable network interface identifier.
///
/// The numeric value is also used as the Linux ifindex exposed by StarryOS.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InterfaceId(u32);

impl InterfaceId {
    pub const LOOPBACK: Self = Self(1);

    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// Convert to Linux ifindex (i32).
    pub const fn to_linux_ifindex(self) -> i32 {
        self.0 as i32
    }

    /// Create from Linux ifindex (i32), rejecting invalid values.
    pub const fn from_linux_ifindex(ifindex: i32) -> Option<Self> {
        if ifindex > 0 {
            Some(Self(ifindex as u32))
        } else {
            None
        }
    }
}

/// Network interface kind.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InterfaceKind {
    Loopback,
    Ethernet,
    /// Layer-3 TUN device: userspace exchanges bare IP packets via a char fd.
    Tun,
    /// Layer-2 TAP device: userspace exchanges Ethernet frames.
    Tap,
}

impl InterfaceKind {
    /// Whether the interface exchanges packets with a userspace `/dev/net/tun`
    /// file descriptor rather than a hardware NIC.
    pub fn is_tuntap(self) -> bool {
        matches!(self, Self::Tun | Self::Tap)
    }
}

bitflags::bitflags! {
    /// Runtime interface flags.
    #[derive(Debug, Clone, Copy, Eq, PartialEq)]
    pub struct InterfaceFlags: u32 {
        const UP = 1 << 0;
        const RUNNING = 1 << 1;
        const LOOPBACK = 1 << 2;
        const BROADCAST = 1 << 3;
        const MULTICAST = 1 << 4;
        /// Point-to-point link (Linux `IFF_POINTOPOINT`). Set on TUN devices
        /// which have no link-layer header and communicate with a single peer.
        const POINTOPOINT = 1 << 5;
        /// ARP disabled (Linux `IFF_NOARP`). Set on TUN devices because layer-3
        /// IP-only interfaces do not use link-layer address resolution.
        const NOARP = 1 << 6;
    }
}

/// Public snapshot of a network interface.
#[derive(Debug, Clone)]
pub struct InterfaceInfo {
    pub id: InterfaceId,
    pub name: String,
    pub kind: InterfaceKind,
    pub mac: Option<EthernetAddress>,
    pub ipv4: Option<Ipv4InterfaceConfig>,
    pub mtu: usize,
    pub flags: InterfaceFlags,
    pub metric: u32,
}

/// Interface matching rule for explicit configuration.
#[derive(Debug, Clone)]
pub enum InterfaceMatcher {
    /// Match the Nth probed Ethernet device.
    ByOrder(usize),
    /// Match a device by its Ethernet MAC address.
    ByMac(EthernetAddress),
    /// Match a device by the name reported by its driver.
    ByDriverName(String),
}

/// Network initialization configuration.
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    /// Per-interface configuration.
    pub interfaces: Vec<InterfaceConfig>,
    /// DNS servers used when no interface-level DNS server is available.
    pub default_dns_servers: Vec<Ipv4Addr>,
}

/// Per-interface network configuration.
#[derive(Debug, Clone)]
pub struct InterfaceConfig {
    /// Public interface name, for example `eth0`.
    pub name: String,
    /// Rule used to bind this config to one probed device.
    pub match_by: InterfaceMatcher,
    /// Static IPv4 configuration. Mutually exclusive with DHCP.
    pub static_ip: Option<StaticIpConfig>,
    /// Whether DHCP client configuration is enabled.
    pub dhcp: bool,
    /// Route metric used for routes installed from this interface.
    pub metric: u32,
    /// Static DNS servers associated with this interface.
    pub dns_servers: Vec<Ipv4Addr>,
}

/// Static IP configuration.
#[derive(Debug, Clone)]
pub struct StaticIpConfig {
    /// IPv4 address assigned to the interface.
    pub ip: Ipv4Addr,
    /// CIDR prefix length.
    pub prefix_len: u8,
    /// Default gateway; `0.0.0.0` means no gateway.
    pub gateway: Ipv4Addr,
}

/// Runtime IPv4 configuration of a network interface.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Ipv4InterfaceConfig {
    /// Interface address and prefix.
    pub address: Ipv4Cidr,
    /// Optional default gateway learned or configured for this interface.
    pub gateway: Option<Ipv4Address>,
}

/// DNS server origin.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DnsSource {
    /// Learned from DHCP.
    Dhcp,
    /// Configured on a matching interface.
    Static,
    /// Global fallback DNS server.
    Fallback,
}

/// Internal DNS server entry with origin metadata.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct DnsServerEntry {
    /// DNS server address.
    pub server: Ipv4Address,
    /// Interface that owns or should route to this server.
    pub interface_id: InterfaceId,
    /// Route/DNS priority; lower values are preferred.
    pub metric: u32,
    /// Source used for priority and reporting decisions.
    pub source: DnsSource,
}

/// Public route snapshot.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RouteInfo {
    /// Destination prefix.
    pub filter: smoltcp::wire::IpCidr,
    /// Optional gateway/next hop.
    pub via: Option<smoltcp::wire::IpAddress>,
    /// Egress interface.
    pub interface_id: InterfaceId,
    /// Source address selected by this route.
    pub source: smoltcp::wire::IpAddress,
    /// Route metric; lower values are preferred.
    pub metric: u32,
}

/// Ordinary socket interface binding.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DeviceBinding {
    /// If set, route selection is constrained to this interface.
    pub bound_if: Option<InterfaceId>,
}
