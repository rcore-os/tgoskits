//! Protocol-side Ethernet frame-port contract.
//!
//! Hardware queue ownership lives in the queue runtime.  The single protocol
//! executor only sees move-only DMA tokens exchanged through bounded SPSC
//! rings; it never calls a NIC queue or an IRQ endpoint directly.

use alloc::{boxed::Box, sync::Arc, vec::Vec};

pub use rd_net::{
    DmaBuffer, RxCompletion, TxChecksumCapabilities, TxChecksumOffload, TxNetworkProtocol,
    TxNotify, TxSubmitOptions, TxTransportProtocol,
};

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

/// Inline compatibility frame used by non-DMA ports and tests.
///
/// Queue-backed ports use the callback methods on [`EthernetFramePort`] so TX
/// is filled directly in DMA storage and RX is consumed before its token is
/// returned. This bounded object remains available for adapters that cannot
/// expose borrowed queue storage.
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

/// Queue-runtime endpoint that accepts an RX DMA token after protocol use.
pub(crate) trait RxBufferRecycler: Send + Sync {
    fn recycle(&self, buffer: DmaBuffer);
}

/// Complete Ethernet frame backed by an owned receive DMA token.
///
/// Dropping this value returns the token to its queue-local recycler. This
/// lets the token remain owned through smoltcp's `RxToken::consume` without
/// exposing NIC queue state to the protocol executor.
pub struct ProtocolRxFrame {
    completion: Option<RxCompletion>,
    recycler: Arc<dyn RxBufferRecycler>,
}

impl ProtocolRxFrame {
    pub(crate) fn new(completion: RxCompletion, recycler: Arc<dyn RxBufferRecycler>) -> Self {
        debug_assert!(completion.packet_len <= completion.buffer.capacity());
        Self {
            completion: Some(completion),
            recycler,
        }
    }

    /// Returns the received L2 frame length excluding FCS.
    pub fn packet_len(&self) -> usize {
        self.completion
            .as_ref()
            .expect("owned RX frame lost its DMA token")
            .packet_len
    }

    /// Borrows the received frame while this value retains DMA ownership.
    pub fn read_with<R>(&self, consume: impl FnOnce(&[u8]) -> R) -> R {
        let completion = self
            .completion
            .as_ref()
            .expect("owned RX frame lost its DMA token");
        completion
            .buffer
            .read_with_cpu(completion.packet_len, consume)
    }
}

impl Drop for ProtocolRxFrame {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            self.recycler.recycle(completion.buffer);
        }
    }
}

/// Device-level protocol endpoint backed by one or more queue-group SPSC
/// pipelines.
pub trait EthernetFramePort: Send + 'static {
    /// Stable portable-driver name used for configuration matching.
    fn device_name(&self) -> &str;

    /// Link-layer address captured during atomic initialization.
    fn mac_address(&self) -> [u8; 6];

    /// Returns transport checksums available on every TX queue of this port.
    fn checksum_capabilities(&self) -> TxChecksumCapabilities {
        TxChecksumCapabilities::NONE
    }

    /// Publishes one complete Ethernet frame to exactly one queue group's
    /// TX-ready ring.
    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult;

    /// Fills a queue-owned DMA token and publishes it for transmission.
    ///
    /// Queue-backed ports override this method so `fill` writes directly into
    /// DMA storage. The compatibility implementation retains the inline frame
    /// path for non-DMA ports and tests.
    fn transmit_frame_with_options(
        &mut self,
        frame_len: usize,
        options: TxSubmitOptions,
        fill: &mut dyn FnMut(&mut [u8]),
    ) -> NetDeviceResult {
        if options.checksum.is_some() {
            return Err(NetDeviceError::InvalidParam);
        }
        let mut frame = ProtocolEthernetFrame::new(frame_len)?;
        fill(frame.packet_mut());
        self.transmit(&frame)
    }

    /// Takes one completed RX frame, if any.
    fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame>;

    /// Takes one completed frame with its DMA ownership token when supported.
    ///
    /// `Ok(None)` selects the compatibility [`receive`](Self::receive) path.
    /// Queue-backed ports return `Err(Again)` while their owned path is idle.
    fn receive_owned(&mut self) -> NetDeviceResult<Option<ProtocolRxFrame>> {
        Ok(None)
    }

    /// Consumes one completed frame while its DMA token is borrowed locally.
    ///
    /// Queue-backed ports override this method to avoid copying into an inline
    /// frame before the protocol adapter consumes it.
    fn receive_with(&mut self, consume: &mut dyn FnMut(&[u8]) -> usize) -> NetDeviceResult<usize> {
        let frame = self.receive()?;
        Ok(consume(frame.packet()))
    }
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
