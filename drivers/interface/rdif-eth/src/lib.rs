#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::ptr::NonNull;

mod owner;

pub use dma_api;
pub use owner::*;
pub use rdif_base::{DriverGeneric, KError, io};
pub use rdif_irq::{
    ContainmentCause, FaultContainment, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource,
};

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
    Other(Box<dyn core::error::Error + Send + Sync>),
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

/// Ownership of packet memory exchanged across the queue boundary.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum QueueMemoryMode {
    /// Hardware directly DMA-accesses the `DmaBuffer` supplied by the runtime.
    ///
    /// The runtime transfers cache ownership to the device before submission
    /// and back to the CPU after RX reclaim. The driver must not perform a
    /// second cache-ownership transfer for the same packet buffer.
    DirectDma,
    /// Only the CPU-side queue owner accesses the supplied `DmaBuffer`.
    ///
    /// The driver copies packet bytes between this buffer and its own private
    /// DMA arena while executing queue methods on the maintenance owner. It may
    /// retain the buffer identity until completion, but hardware must never be
    /// programmed with its `bus_addr`. The runtime therefore performs no DMA
    /// cache synchronization for this buffer.
    OwnerCopy,
}

impl QueueMemoryMode {
    /// Returns whether rd-net owns cache synchronization for packet buffers.
    pub const fn requires_runtime_dma_sync(self) -> bool {
        matches!(self, Self::DirectDma)
    }
}

/// Queue configuration needed by the upper layer packet buffer pool.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct QueueConfig {
    /// DMA addressing mask for the device.
    pub dma_mask: u64,

    /// Required alignment for buffer addresses (in bytes).
    pub align: usize,

    /// DMA packet buffer size in bytes.
    pub buf_size: usize,

    /// Descriptor ring size.
    pub ring_size: usize,

    /// Explicit packet-memory ownership contract for this queue.
    pub memory_mode: QueueMemoryMode,
}

/// Packet buffer passed from the runtime queue layer to a driver queue.
///
/// Whether `bus_addr` may be programmed into hardware is determined by the
/// queue's mandatory [`QueueMemoryMode`].
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
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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

/// Stable interrupt facts captured before the hard handler returns.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Event {
    /// Bitmask of TX queue IDs that have completion events.
    pub tx_queue: IdList,
    /// Bitmask of RX queue IDs that have completion events.
    pub rx_queue: IdList,
    /// Driver-specific status retained for owner-thread service.
    ///
    /// Queue bits remain the portable fast path. A controller that must defer
    /// link, error, or descriptor bookkeeping to its maintenance owner stores
    /// the acknowledged raw status here instead of doing that work in IRQ
    /// context.
    pub device_status: u64,
}

impl Event {
    pub const fn none() -> Self {
        Self {
            tx_queue: IdList::none(),
            rx_queue: IdList::none(),
            device_status: 0,
        }
    }

    /// Returns whether this snapshot contains no device-owned facts.
    pub const fn is_empty(self) -> bool {
        self.tx_queue.0 == 0 && self.rx_queue.0 == 0 && self.device_status == 0
    }
}

/// Portable network IRQ-capture failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EthernetIrqFault {
    /// The endpoint could not read or acknowledge a stable device status.
    #[error("ethernet IRQ status capture failed")]
    Capture,
    /// The endpoint could not mask its exact device source.
    #[error("ethernet IRQ source containment failed")]
    Containment,
}

/// Boxed owned IRQ endpoint.
pub type BIrqEndpoint = Box<dyn IrqEndpoint<Event = Event, Fault = EthernetIrqFault>>;

/// One owner-thread activation delivered to a discovered network interface.
///
/// A hardware event is present only after the registered IRQ endpoint has
/// destructively captured and acknowledged it. A deadline-only activation has
/// `event == None`; it permits an eventless initialization transition but is
/// never evidence that an I/O request completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerInitInput {
    /// Current absolute monotonic time in nanoseconds.
    pub now_ns: u64,
    /// One stable device event captured by the interface IRQ endpoint.
    pub event: Option<Event>,
}

impl OwnerInitInput {
    /// Creates an owner activation with no captured hardware event.
    pub const fn at(now_ns: u64) -> Self {
        Self {
            now_ns,
            event: None,
        }
    }

    /// Creates an owner activation carrying one captured hardware event.
    pub const fn with_event(now_ns: u64, event: Event) -> Self {
        Self {
            now_ns,
            event: Some(event),
        }
    }
}

/// Driver-defined interrupt sources that can advance interface initialization.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InitIrqSources(u64);

impl InitIrqSources {
    /// No hardware source can advance the current state.
    pub const NONE: Self = Self(0);

    /// Creates a driver-defined source set.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Returns the driver-defined source bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Reports whether no hardware source can advance the current state.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Next activation requested by an interface initialization state machine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OwnerInitSchedule {
    /// The state machine has another bounded in-memory transition ready.
    pub run_again: bool,
    /// Device-specific source bits whose IRQ events can advance the state.
    ///
    /// This is an owner/driver scheduling contract, not an OS-side event
    /// filter: the move-only IRQ endpoint remains responsible for capturing
    /// only its device and source generation. A non-zero value proves that an
    /// indefinite wait has a hardware activation source.
    pub irq_sources: InitIrqSources,
    /// Absolute time at which an eventless transition or watchdog may run.
    pub wake_at_ns: Option<u64>,
}

/// Invalid owner-initialization activation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OwnerInitScheduleError {
    /// The state machine cannot make progress and declared no future trigger.
    #[error("owner initialization has no activation source")]
    NoActivationSource,
}

impl OwnerInitSchedule {
    /// Requests one more bounded owner pass without waiting for an event.
    pub const fn run_again() -> Self {
        Self {
            run_again: true,
            irq_sources: InitIrqSources::NONE,
            wake_at_ns: None,
        }
    }

    /// Waits for one or more exact device interrupt sources.
    pub const fn wait_for_irq(irq_sources: InitIrqSources) -> Self {
        Self {
            run_again: false,
            irq_sources,
            wake_at_ns: None,
        }
    }

    /// Waits until an absolute monotonic deadline.
    pub const fn wait_until(wake_at_ns: u64) -> Self {
        Self {
            run_again: false,
            irq_sources: InitIrqSources::NONE,
            wake_at_ns: Some(wake_at_ns),
        }
    }

    /// Waits for either an exact IRQ source or an absolute deadline.
    pub const fn wait_for_irq_until(irq_sources: InitIrqSources, wake_at_ns: u64) -> Self {
        Self {
            run_again: false,
            irq_sources,
            wake_at_ns: Some(wake_at_ns),
        }
    }

    /// Validates that a pending state has at least one future activation.
    pub const fn validate(self) -> Result<Self, OwnerInitScheduleError> {
        if self.run_again || !self.irq_sources.is_empty() || self.wake_at_ns.is_some() {
            Ok(self)
        } else {
            Err(OwnerInitScheduleError::NoActivationSource)
        }
    }
}

/// Result of one bounded owner-side initialization pass.
#[derive(Debug)]
pub enum OwnerInitPoll {
    /// Controller, protocol and policy initialization are complete.
    Ready,
    /// The state machine needs another explicit owner activation.
    Pending(OwnerInitSchedule),
    /// Initialization reached a terminal device error.
    Failed(NetError),
}

// ---------------------------------------------------------------------------
// Optional wireless owner control
// ---------------------------------------------------------------------------

/// One owned link-layer command transferred to the device maintenance owner.
///
/// The values contain no OS objects or borrowed memory. A runtime may therefore
/// queue the command before the portable driver accepts it, while the driver
/// remains independent from threads, wakers, and blocking policy.
#[derive(Debug, Eq, PartialEq)]
pub enum WifiCommand {
    /// Associates the device with an infrastructure network.
    JoinStation {
        /// Raw IEEE 802.11 SSID bytes.
        ssid: Vec<u8>,
        /// Authentication passphrase bytes. An empty value selects an open
        /// network when supported by the device.
        passphrase: Vec<u8>,
    },
    /// Starts an open software access point on one radio channel.
    StartAccessPoint {
        /// Raw IEEE 802.11 SSID bytes.
        ssid: Vec<u8>,
        /// Device channel number.
        channel: u8,
    },
}

/// Terminal result returned by one accepted wireless owner command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiCommandResult {
    /// Station association and its required firmware confirmations completed.
    StationConnected,
    /// Access-point creation and its required firmware confirmations completed.
    AccessPointStarted,
}

/// Next explicit activation required by an accepted wireless owner command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WifiCommandSchedule {
    /// The command has another bounded in-memory transition ready.
    pub run_again: bool,
    /// Device-defined IRQ sources that can advance the command.
    pub irq_sources: InitIrqSources,
    /// Absolute monotonic deadline for an eventless transition or watchdog.
    pub wake_at_ns: Option<u64>,
}

/// Invalid wireless command schedule returned by a portable driver.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WifiCommandScheduleError {
    /// The command cannot progress and declared no future activation.
    #[error("wireless command has no activation source")]
    NoActivationSource,
}

impl WifiCommandSchedule {
    /// Requests one more bounded owner pass without waiting.
    pub const fn run_again() -> Self {
        Self {
            run_again: true,
            irq_sources: InitIrqSources::NONE,
            wake_at_ns: None,
        }
    }

    /// Waits for one or more exact device IRQ sources.
    pub const fn wait_for_irq(irq_sources: InitIrqSources) -> Self {
        Self {
            run_again: false,
            irq_sources,
            wake_at_ns: None,
        }
    }

    /// Waits until an absolute monotonic deadline.
    pub const fn wait_until(wake_at_ns: u64) -> Self {
        Self {
            run_again: false,
            irq_sources: InitIrqSources::NONE,
            wake_at_ns: Some(wake_at_ns),
        }
    }

    /// Waits for either an exact IRQ source or an absolute deadline.
    pub const fn wait_for_irq_until(irq_sources: InitIrqSources, wake_at_ns: u64) -> Self {
        Self {
            run_again: false,
            irq_sources,
            wake_at_ns: Some(wake_at_ns),
        }
    }

    /// Validates that a pending command names at least one future activation.
    pub const fn validate(self) -> Result<Self, WifiCommandScheduleError> {
        if self.run_again || !self.irq_sources.is_empty() || self.wake_at_ns.is_some() {
            Ok(self)
        } else {
            Err(WifiCommandScheduleError::NoActivationSource)
        }
    }
}

/// Result of one bounded owner-side wireless command pass.
#[derive(Debug)]
pub enum WifiCommandProgress {
    /// The accepted command completed successfully.
    Complete(WifiCommandResult),
    /// The accepted command needs another explicit activation.
    Pending(WifiCommandSchedule),
    /// The accepted command reached a terminal device error.
    Failed(NetError),
}

/// Failure to transfer a new command into the portable owner state machine.
#[derive(Debug, thiserror::Error)]
pub enum WifiCommandStartError {
    /// This interface has no wireless control capability.
    #[error("wireless control is not supported")]
    Unsupported(WifiCommand),
    /// Another command is still owned by the driver.
    #[error("wireless control already has an active command")]
    Busy(WifiCommand),
}

/// Core interface that network device drivers must implement.
///
/// Provides device-level operations: queue creation, interrupt management,
/// and MAC address retrieval. Individual packet I/O goes through the queue
/// traits ([`ITxQueue`] / [`IRxQueue`]).
pub trait Interface: DriverGeneric {
    /// Advances discovery-to-ready state on the final CPU-pinned owner.
    ///
    /// The runtime registers and enables the OS IRQ action before the first
    /// call. The portable driver remains responsible for keeping its exact
    /// device sources masked until the corresponding state can consume them.
    /// Implementations must be bounded and must not sleep, busy-wait, create a
    /// task, or access an OS scheduler object.
    ///
    /// The runtime delivers every captured event exactly once. If capture also
    /// masked a device source, the runtime rearms its generation only after
    /// this method accepts the event by returning `Pending` or `Ready`. A
    /// `Failed` result leaves the source masked and enters fail-closed teardown.
    fn poll_owner_init(&mut self, _input: OwnerInitInput) -> OwnerInitPoll {
        OwnerInitPoll::Ready
    }

    /// Returns the device's 6-byte MAC address.
    fn mac_address(&self) -> [u8; 6];

    /// Create a new transmit queue. Returns `None` if no more queues are
    /// available.
    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>>;

    /// Create a new receive queue. Returns `None` if no more queues are
    /// available.
    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>>;

    /// Enables the exact device interrupt sources owned by this interface.
    ///
    /// Failure must remain visible to the runtime: enabling the backing OS
    /// action after this operation failed can leave a shared IRQ line exposed
    /// to an uncontained source.
    fn enable_irq(&mut self) -> Result<(), NetError>;

    /// Masks the exact device interrupt sources owned by this interface.
    ///
    /// The runtime must not continue queue activation or action enablement
    /// unless this operation succeeds.
    fn disable_irq(&mut self) -> Result<(), NetError>;

    /// Check whether device interrupts are currently enabled.
    fn is_irq_enabled(&self) -> bool;

    /// Detach an owned IRQ endpoint from the interface.
    ///
    /// Returns `None` for devices without an OS-registered NIC IRQ. Drivers
    /// with an IRQ line must return `Some` so hard IRQ callbacks do not need to
    /// lock the whole device.
    fn take_irq_endpoint(&mut self) -> Option<BIrqEndpoint> {
        None
    }

    /// Advances controller bookkeeping from an acknowledged IRQ snapshot.
    ///
    /// Only the CPU-pinned maintenance owner may call this method. Queue
    /// completion, descriptor reclaim, link handling, and arbitrary wakeups
    /// belong here rather than in [`IrqEndpoint::capture`].
    fn service_irq_event(&mut self, _event: Event) -> Result<(), NetError> {
        Ok(())
    }

    /// Rearms an exact source that capture deliberately left masked.
    fn rearm_irq_source(&mut self, _source: MaskedSource) -> Result<(), NetError> {
        Err(NetError::NotSupported)
    }

    /// Optional immutable network policy established before publication.
    ///
    /// Wireless control commands after publication use the maintenance
    /// owner's command mailbox; callers never receive a mutable driver object.
    fn owner_link_policy(&self) -> Option<WifiLinkPolicy> {
        None
    }

    /// Reports whether this interface accepts wireless owner commands.
    fn supports_wifi_control(&self) -> bool {
        false
    }

    /// Transfers one command into the CPU-pinned owner's bounded state machine.
    ///
    /// A successful return consumes `command` exactly once. Unsupported and
    /// busy implementations return its ownership in [`WifiCommandStartError`].
    /// The method must not sleep, busy-wait, create tasks, or call an OS wake
    /// primitive.
    fn start_wifi_command(
        &mut self,
        command: WifiCommand,
        _now_ns: u64,
    ) -> Result<WifiCommandProgress, WifiCommandStartError> {
        Err(WifiCommandStartError::Unsupported(command))
    }

    /// Advances the currently accepted wireless command once.
    ///
    /// The maintenance owner calls this only after the preceding schedule's
    /// `run_again`, an acknowledged IRQ event consumed by
    /// [`Interface::service_irq_event`], or its absolute deadline. It is not a
    /// completion-polling API and must remain bounded.
    fn poll_wifi_command(&mut self, _now_ns: u64) -> WifiCommandProgress {
        WifiCommandProgress::Failed(NetError::NotSupported)
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
    /// contain the packet to be transmitted. `Ok(())` transfers the buffer
    /// identity to the queue until [`Self::reclaim`] returns it. `Err` means
    /// the queue and hardware retained no ownership, so the caller may retry
    /// or reclaim the same buffer immediately.
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
    /// `Ok(())` transfers the buffer identity to the queue until
    /// [`Self::reclaim`] returns it. `Err` means the queue and hardware
    /// retained no ownership, so the caller remains responsible for the same
    /// buffer.
    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError>;

    /// Reclaim the next completed receive buffer.
    ///
    /// Returns the buffer bus address and the received byte count.
    fn reclaim(&mut self) -> Option<(u64, usize)>;
}
