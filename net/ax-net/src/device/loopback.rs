//! Loopback device marker.
//!
//! Loopback traffic is handled by the router fast path rather than by device
//! workers. This device still exists so the control plane can expose `lo` as a
//! normal interface and route local packets through the same route table.
//!
//! # Fast Path
//!
//! `Router::dispatch()` copies loopback packets directly from the smoltcp TX
//! buffer into the smoltcp-facing RX buffer. That avoids an extra queue hop and
//! avoids spawning RX/TX workers for a device that has no hardware latency.

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
    ) -> usize {
        // Loopback uses fast path: packets go directly to RouterQueues::rx
        // This recv() is never called by device workers
        0
    }

    fn send(&mut self, _next_hop: IpAddress, _packet: &[u8], _timestamp: Instant) -> usize {
        // Loopback packets are injected directly in Router::dispatch() —
        // this method is never called. The router's loopback fast path
        // calls count_tx()/count_rx() directly on the DeviceHandle.
        unreachable!("loopback Device::send() is never called; router uses fast path")
    }
}
