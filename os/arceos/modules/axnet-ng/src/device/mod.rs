use alloc::{string::String, vec::Vec};
use core::task::Waker;

use smoltcp::{
    storage::PacketBuffer,
    time::Instant,
    wire::{IpAddress, Ipv4Cidr},
};

mod driver;
mod ethernet;
mod loopback;
#[cfg(feature = "vsock")]
mod vsock;

pub use driver::*;
pub use ethernet::*;
pub use loopback::*;
#[cfg(feature = "vsock")]
pub use vsock::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArpEntry {
    pub ip_addr: [u8; 4],
    pub hw_type: u16,
    pub flags: u16,
    pub hw_addr: [u8; 6],
    pub device: String,
}

pub trait Device: Send + Sync {
    fn name(&self) -> &str;

    fn recv(
        &mut self,
        buffer: &mut PacketBuffer<()>,
        timestamp: Instant,
        snoop: &mut dyn FnMut(&[u8]),
    ) -> bool;
    /// Sends a packet to the next hop.
    ///
    /// Returns `true` if this operation resulted in the readiness of receive
    /// operation. This is true for loopback devices and can be used to speed
    /// up packet processing.
    fn send(&mut self, next_hop: IpAddress, packet: &[u8], timestamp: Instant) -> bool;

    fn set_ipv4_addr(&mut self, _addr: Option<Ipv4Cidr>) {}

    fn arp_entries(&self, _timestamp: Instant) -> Vec<ArpEntry> {
        Vec::new()
    }

    fn register_waker(&self, waker: &Waker);
}
