//! [ArceOS](https://github.com/arceos-org/arceos) network module.
//!
//! It provides unified networking primitives for TCP/UDP communication
//! using various underlying network stacks. Currently, only [smoltcp] is
//! supported.
//!
//! # Organization
//!
//! - [`TcpSocket`]: A TCP socket that provides POSIX-like APIs.
//! - [`UdpSocket`]: A UDP socket that provides POSIX-like APIs.
//! - [`dns_query`]: Function for DNS query.
//!
//! # Cargo Features
//!
//! - `smoltcp`: Use [smoltcp] as the underlying network stack. This is enabled
//!   by default.
//!
//! [smoltcp]: https://github.com/smoltcp-rs/smoltcp

#![no_std]

extern crate alloc;
#[cfg(feature = "smoltcp")]
#[macro_use]
extern crate log;

#[cfg(feature = "smoltcp")]
use alloc::{boxed::Box, vec::Vec};

cfg_if::cfg_if! {
    if #[cfg(feature = "smoltcp")] {
        mod smoltcp_impl;
        use smoltcp_impl as net_impl;
    }
}

#[cfg(feature = "smoltcp")]
pub use ax_net_ng::{
    EthernetDriver, NetDeviceError, NetDeviceResult, NetIrqEvents, NetRxBuffer, NetTxBuffer,
    RdNetDriver,
};

#[cfg(feature = "smoltcp")]
pub use self::net_impl::{
    TcpSocket, UdpSocket, bench_receive, bench_transmit, dns_query, poll_interfaces,
};

/// Initializes the network subsystem by NIC devices.
#[cfg(feature = "smoltcp")]
pub fn init_network(mut net_devs: Vec<Box<dyn EthernetDriver>>) {
    info!("Initialize network subsystem...");

    if let Some(dev) = net_devs.pop() {
        info!("  use NIC 0: {:?}", dev.device_name());
        net_impl::init(dev);
    } else {
        warn!("  No network device found!");
    }
}
