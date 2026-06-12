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
    ByOrder(usize),
    ByMac(EthernetAddress),
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
    pub name: String,
    pub match_by: InterfaceMatcher,
    pub static_ip: Option<StaticIpConfig>,
    pub dhcp: bool,
    pub metric: u32,
    pub dns_servers: Vec<Ipv4Addr>,
}

/// Static IP configuration.
#[derive(Debug, Clone)]
pub struct StaticIpConfig {
    pub ip: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Ipv4Addr,
}

/// Runtime IPv4 configuration of a network interface.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Ipv4InterfaceConfig {
    pub address: Ipv4Cidr,
    pub gateway: Option<Ipv4Address>,
}

/// DNS server origin.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum DnsSource {
    Dhcp,
    Static,
    Fallback,
}

/// Internal DNS server entry with origin metadata.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct DnsServerEntry {
    pub server: Ipv4Address,
    pub interface_id: InterfaceId,
    pub metric: u32,
    pub source: DnsSource,
}

/// Public route snapshot.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct RouteInfo {
    pub filter: smoltcp::wire::IpCidr,
    pub via: Option<smoltcp::wire::IpAddress>,
    pub interface_id: InterfaceId,
    pub source: smoltcp::wire::IpAddress,
    pub metric: u32,
}

/// Ordinary socket interface binding.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DeviceBinding {
    pub bound_if: Option<InterfaceId>,
}
