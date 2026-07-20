//! Driver-facing network device contracts.
//!
//! This module is the boundary between ax-net and low-level NIC drivers. It
//! keeps the protocol stack independent from concrete transports by exposing
//! small RX/TX buffer traits, task-context readiness, and an Ethernet facade
//! consumed by higher-level device adapters.
//!
//! # Ownership Model
//!
//! Drivers own their DMA rings or transport queues. ax-net borrows one RX or TX
//! buffer at a time, fills or reads the packet bytes, and then returns control
//! to the driver through transmit/recycle calls. This avoids baking one NIC
//! descriptor model into the protocol stack.
//!
//! # Error Mapping
//!
//! `NetDeviceError` is intentionally small. Device adapters should translate
//! driver-specific failures into retry, bad-state, unsupported, or I/O classes
//! and keep policy decisions such as packet drops at the adapter/router layer.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::num::NonZeroU64;

use axpoll::PollSet;
/// Minimum Ethernet frame payload length (RFC 894).
///
/// Short frames are padded to this length on the wire by the driver's
/// `transmit()` path. The L2 frame byte counts reported through
/// `/proc/net/dev` should reflect the actual on-wire length, including
/// padding.
pub(crate) const ETH_ZLEN: usize = 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetDeviceError {
    /// Operation should be retried later.
    Again,
    /// Device is not in a state that can perform the operation.
    BadState,
    /// Caller supplied an invalid size or argument.
    InvalidParam,
    /// Driver or transport I/O failed.
    Io,
    /// Driver could not allocate required resources.
    NoMemory,
    /// Operation is not supported by this device.
    Unsupported,
}

pub type NetDeviceResult<T = ()> = Result<T, NetDeviceError>;

/// Owned link-layer request accepted by a wireless runtime facade.
#[derive(Debug, Eq, PartialEq)]
pub enum WifiControlCommand {
    /// Associates with an infrastructure network.
    JoinStation {
        /// Raw IEEE 802.11 SSID bytes.
        ssid: Vec<u8>,
        /// Authentication passphrase bytes.
        passphrase: Vec<u8>,
    },
    /// Starts an open software access point.
    StartAccessPoint {
        /// Raw IEEE 802.11 SSID bytes.
        ssid: Vec<u8>,
        /// Device channel number.
        channel: u8,
    },
}

/// Terminal link-layer result of one wireless runtime command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiControlResult {
    /// Station association completed.
    StationConnected,
    /// Access-point creation completed.
    AccessPointStarted,
}

/// FIFO generation assigned by the maintenance owner to one accepted command.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WifiControlGeneration(NonZeroU64);

impl WifiControlGeneration {
    /// Creates a generation from the runtime's non-zero monotonic sequence.
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Returns the numeric generation for ordered protocol-plane commit.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Generation-scoped completion of one wireless link-layer transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiControlCompletion {
    /// FIFO generation assigned at mailbox admission.
    pub generation: WifiControlGeneration,
    /// Terminal link-layer state installed by the maintenance owner.
    pub result: WifiControlResult,
}

/// Immutable task-context facade for a CPU-pinned wireless maintenance owner.
///
/// Implementations may block the calling task while the owner advances a
/// command from IRQ events or absolute deadlines. They must never expose the
/// mutable portable driver or hardware endpoints to the caller.
pub trait WifiControl: Send + Sync {
    /// Transfers one owned command and waits for its generation-matched reply.
    fn reconfigure(&self, command: WifiControlCommand) -> NetDeviceResult<WifiControlCompletion>;
}

/// Receive buffer returned by a low-level driver.
pub trait NetRxBuffer: Send {
    /// Returns the packet bytes received from the device.
    ///
    /// The returned slice MUST represent the Ethernet frame **excluding** the
    /// trailing 4-byte FCS (Frame Check Sequence). The caller (e.g.,
    /// [`EthernetDevice`]) uses this length directly as the L2 frame byte count
    /// for `/proc/net/dev` statistics aligned with Linux semantics.
    fn packet(&self) -> &[u8];
    /// Returns the packet length.
    fn packet_len(&self) -> usize {
        self.packet().len()
    }
}

/// Transmit buffer allocated by a low-level driver.
pub trait NetTxBuffer: Send {
    /// Returns the current packet contents.
    fn packet(&self) -> &[u8];
    /// Returns writable packet storage.
    fn packet_mut(&mut self) -> &mut [u8];
    /// Returns the packet length requested at allocation time.
    fn packet_len(&self) -> usize;
}

/// Minimal Ethernet driver contract consumed by [`EthernetDevice`].
///
/// This is deliberately a runtime facade rather than a hardware-driver trait.
/// The OS runtime owns the real NIC, its IRQ endpoint, queues, and fixed CPU
/// maintenance thread. Implementations of this trait only exchange owned
/// packets with bounded software mailboxes, so ax-net never registers an IRQ
/// or reaches MMIO/descriptor state from its protocol workers.
pub trait EthernetDriver: Send + Sync {
    /// Stable human-readable device name.
    fn device_name(&self) -> &str;
    /// Returns the device MAC address.
    fn mac_address(&self) -> [u8; 6];
    /// Returns task-context readiness published by the runtime owner.
    ///
    /// A hardware-backed facade must return `Some`. Inline software devices
    /// may return `None` when receiving can never become ready asynchronously.
    fn readiness_poll(&self) -> Option<Arc<PollSet>> {
        None
    }
    /// Returns an immutable wireless owner facade when supported.
    fn wifi_control(&self) -> Option<Arc<dyn WifiControl>> {
        None
    }
    /// Allocates a TX buffer large enough for one Ethernet frame.
    fn alloc_tx_buffer(&mut self, size: usize) -> NetDeviceResult<Box<dyn NetTxBuffer>>;
    /// Reclaims completed TX buffers owned by the driver.
    fn recycle_tx_buffers(&mut self) -> NetDeviceResult;
    /// Submits one filled TX buffer.
    fn transmit(&mut self, tx_buf: &mut dyn NetTxBuffer) -> NetDeviceResult;
    /// Receives one packet, or returns [`NetDeviceError::Again`] when idle.
    fn receive(&mut self) -> NetDeviceResult<Box<dyn NetRxBuffer>>;
    /// Returns an RX buffer to the driver.
    fn recycle_rx_buffer(&mut self, rx_buf: &mut dyn NetRxBuffer) -> NetDeviceResult;
}

/// List of Ethernet drivers handed to network initialization.
pub type EthernetDeviceList = Vec<Box<dyn EthernetDriver>>;
