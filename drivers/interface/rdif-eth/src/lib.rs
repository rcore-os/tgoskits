#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::ptr::NonNull;

pub use dma_api;
pub use rdif_base::{DriverGeneric, KError, io};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can occur during network device operations.
#[derive(thiserror::Error, Debug)]
pub enum NetError {
    /// The requested operation is not supported by the device.
    #[error("Operation not supported")]
    NotSupported,

    /// The operation should be retried later (e.g. queue full).
    #[error("Operation should be retried")]
    Retry,

    /// Insufficient memory to complete the operation.
    #[error("Insufficient memory")]
    NoMemory,

    /// The network link is down.
    #[error("Link down")]
    LinkDown,

    /// An unspecified error occurred.
    #[error("Other error: {0}")]
    Other(Box<dyn core::error::Error>),
}

impl From<NetError> for io::ErrorKind {
    fn from(value: NetError) -> Self {
        match value {
            NetError::NotSupported => io::ErrorKind::Unsupported,
            NetError::Retry => io::ErrorKind::Interrupted,
            NetError::NoMemory => io::ErrorKind::OutOfMemory,
            NetError::LinkDown => io::ErrorKind::NotAvailable,
            NetError::Other(e) => io::ErrorKind::Other(e),
        }
    }
}

impl From<dma_api::DmaError> for NetError {
    fn from(value: dma_api::DmaError) -> Self {
        match value {
            dma_api::DmaError::NoMemory => NetError::NoMemory,
            e => NetError::Other(Box::new(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// DMA buffer helpers
// ---------------------------------------------------------------------------

/// Queue configuration needed by the upper layer DMA pool.
#[derive(Debug, Clone, Copy)]
pub struct QueueConfig {
    /// DMA addressing mask for the device.
    pub dma_mask: u64,

    /// Required alignment for buffer addresses (in bytes).
    pub align: usize,

    /// DMA packet buffer size in bytes.
    pub buf_size: usize,

    /// Descriptor ring size.
    pub ring_size: usize,
}

/// DMA buffer passed from the runtime queue layer to a driver queue.
#[derive(Clone, Copy, Debug)]
pub struct DmaBuffer {
    /// CPU virtual address for drivers that need to build descriptors from a
    /// slice or write transport-specific headers.
    pub virt: NonNull<u8>,
    /// Device-visible DMA address for hardware descriptors.
    pub bus_addr: u64,
    /// Buffer length in bytes.
    pub len: usize,
}

/// Bitmask tracking up to 64 queue identifiers.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct IdList(u64);

impl IdList {
    pub const fn none() -> Self {
        Self(0)
    }

    pub fn contains(&self, id: usize) -> bool {
        (self.0 & (1 << id)) != 0
    }

    pub fn insert(&mut self, id: usize) {
        self.0 |= 1 << id;
    }

    pub fn remove(&mut self, id: usize) {
        self.0 &= !(1 << id);
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> {
        let bits = self.0;
        (0..64).filter(move |i| (bits & (1 << i)) != 0)
    }
}

/// Event returned by [`Interface::handle_irq`] indicating which queues have
/// completed operations.
#[derive(Debug, Clone, Copy)]
pub struct Event {
    /// Bitmask of TX queue IDs that have completion events.
    pub tx_queue: IdList,
    /// Bitmask of RX queue IDs that have completion events.
    pub rx_queue: IdList,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            tx_queue: IdList::none(),
            rx_queue: IdList::none(),
        }
    }
}

/// Owned interrupt endpoint for a network device.
///
/// Drivers that can split their control/data-plane state from the IRQ
/// top-half should return this through [`Interface::take_irq_handler`]. The
/// handler is then moved into the platform IRQ callback, so hard IRQ context no
/// longer needs to lock the complete network device object.
pub trait IrqHandler: Send + 'static {
    /// Acknowledge/snapshot the device IRQ source and publish queue-local event
    /// bits. Packet copies, descriptor refills, DMA reclaim, and waker
    /// execution must stay in task/deferred context.
    fn handle_irq(&mut self) -> Event;
}

/// Boxed owned IRQ endpoint.
pub type BIrqHandler = Box<dyn IrqHandler>;

/// Core interface that network device drivers must implement.
///
/// Provides device-level operations: queue creation, interrupt management,
/// and MAC address retrieval. Individual packet I/O goes through the queue
/// traits ([`ITxQueue`] / [`IRxQueue`]).
pub trait Interface: DriverGeneric {
    /// Returns the device's 6-byte MAC address.
    fn mac_address(&self) -> [u8; 6];

    /// Create a new transmit queue. Returns `None` if no more queues are
    /// available.
    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>>;

    /// Create a new receive queue. Returns `None` if no more queues are
    /// available.
    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>>;

    /// Enable device interrupts.
    fn enable_irq(&mut self);

    /// Disable device interrupts.
    fn disable_irq(&mut self);

    /// Check whether device interrupts are currently enabled.
    fn is_irq_enabled(&self) -> bool;

    /// Handle a device interrupt, returning which queues have events.
    fn handle_irq(&mut self) -> Event;

    /// Detach an owned IRQ endpoint from the interface.
    ///
    /// The default returns `None` to keep older drivers on the compatibility
    /// path, where [`Interface::handle_irq`] is still called through the shared
    /// interface object. New or migrated drivers should return `Some` so hard
    /// IRQ callbacks do not need to lock the whole device.
    fn take_irq_handler(&mut self) -> Option<BIrqHandler> {
        None
    }

    /// Optional wireless control plane.
    ///
    /// A plain wired NIC returns `None` (the default). A wireless device
    /// returns its [`WifiControl`] so the upper layers can drive STA/SoftAP
    /// control, link policy and out-of-band RX wake-up through the *same* net
    /// device model — no separate Wi-Fi device type or registration path.
    fn wifi_control(&mut self) -> Option<&mut dyn WifiControl> {
        None
    }
}

// ---------------------------------------------------------------------------
// Optional wireless control plane
// ---------------------------------------------------------------------------

/// Wireless link policy a device reports for itself, so the protocol stack can
/// apply it without any Wi-Fi/SoftAP-specific knowledge.
///
/// This is plain data carried alongside the device; the stack only sees a
/// static IPv4 + optional single-client DHCP server lease.
#[derive(Clone, Copy, Debug)]
pub struct WifiLinkPolicy {
    /// This interface's static address / SoftAP gateway.
    pub ip: [u8; 4],
    /// Prefix length for [`ip`](Self::ip).
    pub prefix_len: u8,
    /// If set, run a built-in DHCP server handing out this single address.
    pub dhcp_server_client_ip: Option<[u8; 4]>,
}

/// Optional control plane for a wireless [`Interface`].
///
/// Bundles the wireless-specific capabilities (STA connect, SoftAP start, MAC,
/// out-of-band RX wake, link policy) onto the same object that carries the data
/// plane. A chip driver implements this on its `Interface` device so wireless
/// devices need no bespoke lifecycle trait or registration path.
pub trait WifiControl {
    /// Connect to a network in STA mode (scan + associate + authenticate).
    fn connect(&mut self, ssid: &str, password: &str) -> Result<(), NetError>;

    /// Disconnect from the current STA network.
    fn disconnect(&mut self) -> Result<(), NetError>;

    /// Start an open (unencrypted) SoftAP broadcasting `ssid` on `channel`.
    fn start_ap_open(&mut self, ssid: &[u8], channel: u8) -> Result<(), NetError>;

    /// Register a wake callback for out-of-band RX.
    ///
    /// SDIO Wi-Fi delivers RX outside the ethernet IRQ framework, so the driver
    /// calls this `wake` when a data frame has been enqueued, to nudge the
    /// stack's per-device poll task.
    fn set_rx_wake(&mut self, wake: fn());

    /// The link policy this device wants applied once the stack is up. `None`
    /// means "no special policy" (e.g. a STA that will use DHCP like any NIC).
    fn link_policy(&self) -> Option<WifiLinkPolicy>;
}

// ---------------------------------------------------------------------------
// Transmit queue
// ---------------------------------------------------------------------------

/// Transmit queue interface.
///
/// A driver creates one or more TX queues via [`Interface::create_tx_queue`]
/// and exchanges DMA buffer bus addresses with the caller.
pub trait ITxQueue: Send + 'static {
    /// Queue identifier (unique within the device).
    fn id(&self) -> usize;

    /// DMA buffer configuration for this queue.
    fn config(&self) -> QueueConfig;

    /// Submit a DMA buffer for transmission.
    ///
    /// `bus_addr` must point to a DMA-capable buffer whose first `len` bytes
    /// contain the packet to be transmitted.
    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError>;

    /// Reclaim the next completed transmit buffer.
    ///
    /// Returns the buffer bus address when the device has completed sending it.
    fn reclaim(&mut self) -> Option<u64>;
}

// ---------------------------------------------------------------------------
// Receive queue
// ---------------------------------------------------------------------------

/// Receive queue interface.
///
/// A driver creates one or more RX queues via [`Interface::create_rx_queue`]
/// and exchanges DMA buffer bus addresses with the caller.
pub trait IRxQueue: Send + 'static {
    /// Queue identifier (unique within the device).
    fn id(&self) -> usize;

    /// DMA buffer configuration for this queue.
    fn config(&self) -> QueueConfig;

    /// Submit an empty DMA buffer to hardware.
    ///
    /// `bus_addr` must point to a DMA-capable buffer whose total size is `len`.
    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError>;

    /// Reclaim the next completed receive buffer.
    ///
    /// Returns the buffer bus address and the received byte count.
    fn reclaim(&mut self) -> Option<(u64, usize)>;
}
