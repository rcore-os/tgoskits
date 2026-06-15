use core::task::Waker;

use smoltcp::{time::Instant, wire::IpAddress};

use crate::{config::InterfaceId, device::Device};

/// Loopback device for local traffic.
///
/// Unlike Ethernet devices, loopback uses a fast path that bypasses device
/// workers: packets are injected directly into the router's RX queue on send.
pub struct LoopbackDevice;

impl LoopbackDevice {
    pub fn new() -> Self {
        Self
    }
}

impl Device for LoopbackDevice {
    fn name(&self) -> &str {
        "lo"
    }

    fn recv(
        &mut self,
        _interface_id: InterfaceId,
        _buffer: &mut smoltcp::storage::PacketBuffer<InterfaceId>,
        _timestamp: Instant,
        _snoop: &mut dyn FnMut(&[u8]),
    ) -> bool {
        // Loopback uses fast path: packets go directly to RouterQueues::rx
        // This recv() is never called by device workers
        false
    }

    fn send(&mut self, _next_hop: IpAddress, _packet: &[u8], _timestamp: Instant) -> bool {
        // Fast path: loopback packets are injected directly in Router::dispatch()
        // See Router::dispatch_loopback()
        true
    }

    fn register_waker(&self, _waker: &Waker) {
        // No async operations needed for loopback fast path
    }
}
