//! Common traits and types for network device (NIC) drivers.

#![no_std]
#![cfg_attr(doc, feature(doc_cfg))]

extern crate alloc;

#[cfg(feature = "fxmac")]
/// fxmac driver for PhytiumPi
pub mod fxmac;
#[cfg(feature = "ixgbe")]
/// ixgbe NIC device driver.
pub mod ixgbe;

#[doc(no_inline)]
pub use axdriver_base::{BaseDriverOps, DevError, DevResult, DeviceType};

mod net_buf;
pub use self::net_buf::{NetBuf, NetBufBox, NetBufPool, NetBufPtr};

/// The ethernet address of the NIC (MAC address).
pub struct EthernetAddress(pub [u8; 6]);

/// Operations that require a network device (NIC) driver to implement.
pub trait NetDriverOps: BaseDriverOps {
    /// The ethernet address of the NIC.
    fn mac_address(&self) -> EthernetAddress;

    /// Whether can transmit packets.
    fn can_transmit(&self) -> bool;

    /// Whether can receive packets.
    fn can_receive(&self) -> bool;

    /// Size of the receive queue.
    fn rx_queue_size(&self) -> usize;

    /// Size of the transmit queue.
    fn tx_queue_size(&self) -> usize;

    /// Gives back the `rx_buf` to the receive queue for later receiving.
    ///
    /// `rx_buf` should be the same as the one returned by
    /// [`NetDriverOps::receive`].
    fn recycle_rx_buffer(&mut self, rx_buf: NetBufPtr) -> DevResult;

    /// Poll the transmit queue and gives back the buffers for previous
    /// transmiting. returns [`DevResult`].
    fn recycle_tx_buffers(&mut self) -> DevResult;

    /// Transmits a packet in the buffer to the network, without blocking,
    /// returns [`DevResult`].
    fn transmit(&mut self, tx_buf: NetBufPtr) -> DevResult;

    /// Receives a packet from the network and store it in the [`NetBuf`],
    /// returns the buffer.
    ///
    /// Before receiving, the driver should have already populated some buffers
    /// in the receive queue by [`NetDriverOps::recycle_rx_buffer`].
    ///
    /// If currently no incomming packets, returns an error with type
    /// [`DevError::Again`].
    fn receive(&mut self) -> DevResult<NetBufPtr>;

    /// Allocate a memory buffer of a specified size for network transmission,
    /// returns [`DevResult`]
    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr>;
}
