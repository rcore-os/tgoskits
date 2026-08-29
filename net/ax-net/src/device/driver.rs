//! Protocol-side Ethernet frame-port contract.
//!
//! Hardware queue ownership lives in the queue runtime.  The single protocol
//! executor only sees move-only DMA tokens exchanged through bounded SPSC
//! rings; it never calls a NIC queue or an IRQ endpoint directly.

use alloc::{boxed::Box, vec::Vec};

/// Minimum Ethernet frame length on the wire, excluding the FCS.
pub(crate) const ETH_ZLEN: usize = 60;
/// Maximum Ethernet frame transferred across the protocol boundary.
pub(crate) const ETHERNET_FRAME_CAPACITY: usize = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NetDeviceError {
    /// The bounded frame ring or DMA token pool is temporarily exhausted.
    #[error("network frame port should be retried")]
    Again,
    /// The port has been stopped and rejects new traffic.
    #[error("network frame port is stopped")]
    Stopped,
    /// Caller supplied a frame size outside the queue contract.
    #[error("invalid network frame size")]
    InvalidParam,
    /// Driver or DMA processing failed.
    #[error("network frame port I/O failed")]
    Io,
    /// A required DMA allocation could not be obtained.
    #[error("network frame port memory allocation failed")]
    NoMemory,
}

pub type NetDeviceResult<T = ()> = Result<T, NetDeviceError>;

/// Inline frame used only by the single protocol executor.
///
/// Queue-facing storage remains a move-only DMA token.  The protocol port
/// copies a bounded frame into this inline object and immediately publishes
/// the token to the recycle SPSC ring, so no heap allocation or shared frame
/// ownership is introduced.
#[derive(Clone)]
pub struct ProtocolEthernetFrame {
    bytes: [u8; ETHERNET_FRAME_CAPACITY],
    len: usize,
}

impl ProtocolEthernetFrame {
    pub fn new(len: usize) -> NetDeviceResult<Self> {
        if len > ETHERNET_FRAME_CAPACITY {
            return Err(NetDeviceError::InvalidParam);
        }
        Ok(Self {
            bytes: [0; ETHERNET_FRAME_CAPACITY],
            len,
        })
    }

    pub fn packet(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub fn packet_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[..self.len]
    }

    pub fn packet_len(&self) -> usize {
        self.len
    }

    pub(crate) fn copy_from_slice(packet: &[u8]) -> NetDeviceResult<Self> {
        let mut frame = Self::new(packet.len())?;
        frame.packet_mut().copy_from_slice(packet);
        Ok(frame)
    }
}

/// Device-level protocol endpoint backed by one or more queue-group SPSC
/// pipelines.
pub trait EthernetFramePort: Send + 'static {
    /// Stable portable-driver name used for configuration matching.
    fn device_name(&self) -> &str;

    /// Link-layer address captured during atomic initialization.
    fn mac_address(&self) -> [u8; 6];

    /// Copies and publishes one complete Ethernet frame to exactly one queue
    /// group's TX-ready ring.
    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult;

    /// Takes one completed RX frame, if any.
    fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame>;
}

/// Protocol endpoints handed to the unique smoltcp owner during startup.
pub type EthernetFramePortList = Vec<Box<dyn EthernetFramePort>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_device_errors_have_domain_messages() {
        assert_eq!(
            alloc::format!("{}", NetDeviceError::NoMemory),
            "network frame port memory allocation failed"
        );
    }
}
