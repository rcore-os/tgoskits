use alloc::vec::Vec;
use core::net::Ipv4Addr;

use smoltcp::wire::{Ipv4Address, Ipv4Cidr};

/// Network initialization configuration.
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    /// Static IP configuration. If None, DHCP will be used.
    pub static_ip: Option<StaticIpConfig>,
    /// DNS servers.
    ///
    /// - In **static IP mode**: these are the primary DNS servers.
    /// - In **DHCP mode**: these are fallback servers, used only if DHCP doesn't provide DNS servers.
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
