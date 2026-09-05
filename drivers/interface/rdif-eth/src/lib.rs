#![no_std]

extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};
use core::{fmt, ptr::NonNull};

pub use dma_api;
pub use rdif_base::{DriverGeneric, KError, io};
use zeroize::Zeroize;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can occur during network device operations.
#[derive(thiserror::Error, Debug)]
pub enum NetError {
    /// The requested operation is not supported by the device.
    #[error("Operation not supported")]
    NotSupported,

    /// The operation can make progress after a queue IRQ or an explicit
    /// task-side device event (for example, a queue-full completion).
    ///
    /// The runtime retains the returned DMA token, rearms the interrupt source,
    /// and does not busy-poll this condition. Drivers must use a different
    /// error when no future event can make the operation ready.
    #[error("Operation should be retried")]
    Retry,

    /// Insufficient memory to complete the operation.
    #[error("Insufficient memory")]
    NoMemory,

    /// The network link is down.
    #[error("Link down")]
    LinkDown,

    /// The device parts or queue topology violate the runtime contract.
    #[error("Invalid network device parts")]
    InvalidParts,

    /// The queue or device has been stopped and rejects new work.
    #[error("Network queue stopped")]
    Stopped,

    /// The device cannot provide the required interrupt/rearm semantics.
    #[error("Required network interrupt semantics are unavailable")]
    IrqUnavailable,

    /// Hardware DMA shutdown could not be confirmed, so device-owned backing
    /// must remain quarantined instead of being released.
    #[error("Network DMA shutdown could not be confirmed")]
    DmaShutdownUnconfirmed,

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
            NetError::InvalidParts => io::ErrorKind::InvalidData,
            NetError::Stopped => io::ErrorKind::NotAvailable,
            NetError::IrqUnavailable => io::ErrorKind::Unsupported,
            NetError::DmaShutdownUnconfirmed => io::ErrorKind::NotAvailable,
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

/// Transport checksums a transmit queue can calculate for complete packets.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TxChecksumCapabilities(u8);

impl TxChecksumCapabilities {
    const TCP: u8 = 1 << 0;
    const UDP: u8 = 1 << 1;

    /// The queue does not calculate transport checksums.
    pub const NONE: Self = Self(0);
    /// The queue calculates TCP and UDP checksums for IPv4 and IPv6 packets.
    pub const TCP_UDP: Self = Self(Self::TCP | Self::UDP);

    /// Retains only checksum operations supported by both queues.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Returns whether TCP checksum calculation is supported.
    pub const fn supports_tcp(self) -> bool {
        self.0 & Self::TCP != 0
    }

    /// Returns whether UDP checksum calculation is supported.
    pub const fn supports_udp(self) -> bool {
        self.0 & Self::UDP != 0
    }
}

/// Network protocol containing a transport checksum requested from hardware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxNetworkProtocol {
    Ipv4,
    Ipv6,
}

/// Transport protocol whose checksum hardware must calculate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TxTransportProtocol {
    Tcp,
    Udp,
}

/// Per-packet transmit checksum request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxChecksumOffload {
    pub network: TxNetworkProtocol,
    pub transport: TxTransportProtocol,
    /// Byte offset of the transport header from the start of the L2 frame.
    pub transport_offset: u16,
}

/// Move-only DMA ownership token passed between a runtime and one driver queue.
///
/// The token uniquely owns CPU access to the mapped range while it is outside
/// the device. Submitting it transfers that ownership to the queue; reclaiming
/// returns the same token. It intentionally does not implement `Clone` or
/// `Copy`.
pub struct DmaBuffer {
    // Keep the move-only queue token small. The backing allocation is boxed
    // once while the runtime constructs its preallocated pools; submit/reclaim
    // and error return paths only move this pointer and never allocate.
    buffer: Box<dma_api::ContiguousBuffer>,
    /// Logical packet/mapping length visible to the current owner.
    len: usize,
}

impl fmt::Debug for DmaBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaBuffer")
            .field("virt", &self.as_ptr())
            .field("bus_addr", &self.bus_addr())
            .field("len", &self.len)
            .field("capacity", &self.capacity())
            .finish()
    }
}

impl DmaBuffer {
    /// Creates a unique DMA ownership token.
    ///
    /// Returns the boxed allocation unchanged when `len` exceeds its capacity.
    /// Boxing happens before queues or IRQs become live; moving the resulting
    /// token across queue boundaries does not allocate.
    pub fn new(
        buffer: dma_api::ContiguousBuffer,
        len: usize,
    ) -> Result<Self, Box<dma_api::ContiguousBuffer>> {
        let buffer = Box::new(buffer);
        if len > buffer.len() {
            return Err(buffer);
        }
        Ok(Self { buffer, len })
    }

    /// Returns the CPU virtual address of the mapped range.
    pub fn as_ptr(&self) -> NonNull<u8> {
        self.buffer.as_ptr()
    }

    /// Returns the device-visible DMA address.
    pub fn bus_addr(&self) -> u64 {
        self.buffer.dma_addr().as_u64()
    }

    /// Returns the mapped range length.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the mapped range is empty.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the allocation capacity.
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Updates the logical length without changing allocation ownership.
    pub fn set_len(&mut self, len: usize) -> Result<(), NetError> {
        if len > self.capacity() {
            return Err(NetError::InvalidParts);
        }
        self.len = len;
        Ok(())
    }

    /// Runs a CPU write closure over the logical prefix.
    pub fn write_with_cpu<R>(&mut self, f: impl FnOnce(&mut [u8]) -> R) -> R {
        self.buffer.write_with_cpu(self.len, f)
    }

    /// Runs a CPU read closure over `len` bytes of the allocation.
    pub fn read_with_cpu<R>(&self, len: usize, f: impl FnOnce(&[u8]) -> R) -> R {
        assert!(len <= self.capacity(), "DMA read exceeds buffer capacity");
        self.buffer.read_with_cpu(len, f)
    }

    /// Publishes the logical prefix to the device for non-coherent DMA.
    pub fn prepare_for_device(&self) {
        self.buffer.prepare_for_device(0..self.len);
    }

    /// Makes a completed device-written prefix visible to the CPU.
    pub fn complete_for_cpu(&self, len: usize) {
        assert!(
            len <= self.capacity(),
            "DMA completion exceeds buffer capacity"
        );
        self.buffer.complete_for_cpu(0..len);
    }
}

/// A queue submit failure that returns the unconsumed DMA token.
#[derive(Debug)]
pub struct SubmitError {
    buffer: DmaBuffer,
    error: NetError,
}

impl SubmitError {
    /// Creates a submit error while returning ownership of `buffer`.
    pub const fn new(buffer: DmaBuffer, error: NetError) -> Self {
        Self { buffer, error }
    }

    /// Returns the operation error.
    pub const fn error(&self) -> &NetError {
        &self.error
    }

    /// Splits the error into the returned token and typed reason.
    pub fn into_parts(self) -> (DmaBuffer, NetError) {
        (self.buffer, self.error)
    }

    /// Returns the unconsumed DMA token.
    pub fn into_buffer(self) -> DmaBuffer {
        self.buffer
    }
}

/// Stable identifier of one runtime poll group within a device.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct NetPollGroupId(u16);

impl NetPollGroupId {
    /// Creates a group identifier.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the raw device-local identifier.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Stable identifier of one hardware queue within a device.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct NetQueueId(u16);

impl NetQueueId {
    /// Creates a queue identifier.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the raw device-local identifier.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Stable identifier used to join endpoints that share one physical IRQ.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub struct NetIrqSourceId(u16);

impl NetIrqSourceId {
    /// Creates an IRQ source identifier.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the driver-local source identifier.
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Bounded hard-IRQ status snapshot for one poll group.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct NetIrqSnapshot(u8);

impl NetIrqSnapshot {
    /// RX completion or receive work is pending.
    pub const RX: Self = Self(1 << 0);
    /// TX completion or transmit progress is pending.
    pub const TX: Self = Self(1 << 1);
    /// A bounded device error/status transition needs task-context handling.
    pub const ERROR: Self = Self(1 << 2);

    /// Creates an empty snapshot.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Returns a snapshot containing all queue work classes.
    pub const fn all_queue_work() -> Self {
        Self(Self::RX.0 | Self::TX.0)
    }

    /// Returns whether `other` is present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Combines two snapshots.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Result of one bounded network hard-IRQ endpoint invocation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NetHardIrqResult {
    /// The shared physical IRQ was not asserted by this endpoint.
    Spurious,
    /// The endpoint masked/acknowledged its source and scheduled this group.
    Schedule(NetIrqSnapshot),
    /// The transport could not be inspected without waiting for task context.
    ProbeDeferred,
}

/// Owned interrupt endpoint for a network device.
///
/// Drivers split this endpoint from control and queue ownership in
/// [`NetDevice::into_parts`]. The handler is moved into the platform IRQ
/// callback, so hard IRQ context never locks the complete network device.
pub trait NetHardIrqHandler: Send + 'static {
    /// Acknowledge/snapshot the device IRQ source and publish queue-local event
    /// bits. Packet copies, descriptor refills, DMA reclaim, and waker
    /// execution must stay in task/deferred context.
    fn handle_irq(&mut self) -> NetHardIrqResult;
}

/// A move-only hard-IRQ endpoint tied to one platform IRQ source.
pub struct NetHardIrqEndpoint {
    source_id: NetIrqSourceId,
    handler: Box<dyn NetHardIrqHandler>,
}

impl NetHardIrqEndpoint {
    /// Creates an endpoint for `source_id`.
    pub fn new(source_id: NetIrqSourceId, handler: Box<dyn NetHardIrqHandler>) -> Self {
        Self { source_id, handler }
    }

    /// Returns the driver-local source identity.
    pub const fn source_id(&self) -> NetIrqSourceId {
        self.source_id
    }

    /// Runs the bounded hard-IRQ endpoint.
    pub fn handle_irq(&mut self) -> NetHardIrqResult {
        self.handler.handle_irq()
    }
}

/// Result of atomically rearming a poll group and checking for new work.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NetRearmResult {
    /// Interrupts are armed and no work was observed.
    Idle,
    /// Work appeared in the rearm window; the group must be repolled.
    WorkPending(NetIrqSnapshot),
    /// The device needs another owner poll at an absolute monotonic deadline.
    RetryAt { deadline_nanos: u64 },
}

/// Task-context interrupt control for exactly one poll group.
pub trait NetPollIrqControl: Send + 'static {
    /// Masks the group's complete IRQ domain and prevents new device work from
    /// being published. This method runs on the group's owner CPU.
    fn quiesce(&mut self) -> Result<(), NetError>;

    /// Stops the group's DMA engine and proves that descriptors and submitted
    /// buffers can be released.
    ///
    /// This method runs on the group's owner CPU after its IRQ registrations
    /// have been disabled and synchronized. Returning an error requires the
    /// runtime to quarantine the complete poll group; a driver must never
    /// report success while hardware can still reach queue memory.
    fn shutdown(&mut self) -> Result<(), NetError>;

    /// Rearms the group and atomically checks the source/queues for work that
    /// appeared in the enable window. This method runs on the owner CPU.
    fn rearm_and_check(&mut self, now_nanos: u64) -> Result<NetRearmResult, NetError>;
}

/// Result of one finite owner-startup step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetOwnerStartupProgress {
    /// Startup completed; queues may now be refilled and published.
    Ready,
    /// Startup needs a hardware interrupt before it can advance.
    WaitForInterrupt,
    /// Startup needs an interrupt, but must also be advanced at this absolute
    /// deadline so a missing interrupt becomes a terminal timeout.
    WaitForInterruptUntil { deadline_nanos: u64 },
    /// Startup needs another owner poll at an absolute monotonic deadline.
    RetryAt { deadline_nanos: u64 },
}

/// Move-only task-context initialization executed by one poll-group owner.
///
/// The runtime invokes this endpoint after the owner worker is pinned and all
/// IRQ callbacks are registered and enabled, but before initial queue refill
/// and queue publication. Probe code may use it to defer firmware and
/// device-control work that must share the queue's fixed CPU ownership. IRQs
/// observed during this phase wake only the startup state machine; normal RX/TX
/// queue polling remains disabled until startup reports [`Ready`](NetOwnerStartupProgress::Ready).
pub trait NetOwnerStartup: Send + 'static {
    /// Begins startup without blocking or polling hardware to completion.
    fn start(&mut self, now_nanos: u64) -> Result<NetOwnerStartupProgress, NetError>;

    /// Advances startup for a hardware notification or elapsed deadline.
    fn advance(&mut self, now_nanos: u64) -> Result<NetOwnerStartupProgress, NetError>;

    /// Cancels startup and stops any in-flight device operation.
    fn cancel(&mut self) -> Result<(), NetError>;
}

/// Result of one finite wireless-control step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WifiControlProgress {
    /// The requested hardware transition completed.
    Complete,
    /// The transition needs a hardware interrupt before it can advance.
    WaitForInterrupt,
    /// The transition needs an interrupt, but must also be advanced at this
    /// absolute deadline so cancellation and timeout remain deterministic.
    WaitForInterruptUntil { deadline_nanos: u64 },
    /// The transition needs another owner poll at an absolute monotonic deadline.
    RetryAt { deadline_nanos: u64 },
}

/// Static information published with one network device.
#[derive(Debug)]
pub struct NetDeviceInfo {
    /// Portable driver name.
    pub driver_name: String,
    /// Initial link-layer address.
    pub mac_address: [u8; 6],
}

impl NetDeviceInfo {
    /// Creates device information.
    pub fn new(driver_name: impl Into<String>, mac_address: [u8; 6]) -> Self {
        Self {
            driver_name: driver_name.into(),
            mac_address,
        }
    }
}

/// Exclusive task-context control endpoint for a network device.
pub trait NetControlEndpoint: Send + 'static {
    /// Returns the current link-layer address.
    fn mac_address(&mut self) -> Result<[u8; 6], NetError>;
}

/// Control endpoint for a device with an immutable link-layer address.
pub struct FixedNetControl {
    mac_address: [u8; 6],
}

impl FixedNetControl {
    /// Creates a fixed control endpoint.
    pub const fn new(mac_address: [u8; 6]) -> Self {
        Self { mac_address }
    }
}

impl NetControlEndpoint for FixedNetControl {
    fn mac_address(&mut self) -> Result<[u8; 6], NetError> {
        Ok(self.mac_address)
    }
}

/// One RX/TX hardware queue pair moved to a poll group owner.
pub struct NetQueuePairParts {
    /// Transmit queue.
    pub tx: Box<dyn ITxQueue>,
    /// Receive queue.
    pub rx: Box<dyn IRxQueue>,
}

/// All driver-owned parts sharing one IRQ mask/rearm domain.
pub struct NetPollGroupParts {
    /// Stable group identifier.
    pub id: NetPollGroupId,
    /// Hardware queues exclusively owned by the group executor.
    pub queues: NetQueuePairParts,
    /// Task-context mask/rearm endpoint.
    pub irq_control: Box<dyn NetPollIrqControl>,
    /// Optional one-shot device initialization owned by this group's CPU.
    pub owner_startup: Option<Box<dyn NetOwnerStartup>>,
    /// One or more hard endpoints whose sources all map to this group.
    pub irq_endpoints: Vec<NetHardIrqEndpoint>,
}

/// Destructured portable network device.
pub struct NetDeviceParts {
    /// Immutable device information.
    pub info: NetDeviceInfo,
    /// Exclusive task-context control endpoint.
    pub control: Box<dyn NetControlEndpoint>,
    /// Optional owned wireless control endpoint.
    pub wifi_control: Option<Box<dyn WifiControl>>,
    /// Every hardware poll group provided by the driver.
    pub poll_groups: Vec<NetPollGroupParts>,
}

/// Portable network device consumed exactly once into independent ownership
/// parts.
pub trait NetDevice: DriverGeneric + Send + 'static {
    /// Consumes the complete device and returns its control, queue and IRQ
    /// ownership parts. No complete-device handle remains after success.
    fn into_parts(self: Box<Self>) -> Result<NetDeviceParts, NetError>;
}

// ---------------------------------------------------------------------------
// Optional wireless control plane
// ---------------------------------------------------------------------------

/// Wireless link policy a device reports for itself, so the protocol stack can
/// apply it without any Wi-Fi/SoftAP-specific knowledge.
///
/// This is plain data carried alongside the device; the stack only sees a
/// static IPv4 + optional single-client DHCP server lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WifiLinkPolicy {
    /// This interface's static address / SoftAP gateway.
    pub ip: [u8; 4],
    /// Prefix length for [`ip`](Self::ip).
    pub prefix_len: u8,
    /// If set, run a built-in DHCP server handing out this single address.
    pub dhcp_server_client_ip: Option<[u8; 4]>,
}

/// One owned wireless control operation.
///
/// Operations are submitted to the queue executor that owns the device's IRQ
/// domain. Callers must never execute them directly on their own CPU.
#[derive(Clone, Eq, PartialEq)]
pub struct Wpa2Pmk([u8; 32]);

impl Wpa2Pmk {
    /// Creates one owned WPA2 pairwise master key.
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrows the PMK for transfer into a device owner.
    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Wpa2Pmk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Wpa2Pmk([REDACTED])")
    }
}

impl Drop for Wpa2Pmk {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum WifiOperation {
    /// Configure station mode and connect to one network.
    Connect {
        /// Network name.
        ssid: String,
        /// Pairwise master key for WPA2, or `None` for an open network.
        pmk: Option<Wpa2Pmk>,
        /// Caller-owned entropy for a secured connection.
        ///
        /// A secured driver must reject the operation when this is `None`;
        /// time values are not an acceptable random source.
        entropy: Option<[u8; 32]>,
    },
    /// Disconnect the current station connection.
    Disconnect,
    /// Configure and start an open access point.
    StartOpenAccessPoint {
        /// Network name bytes advertised by the access point.
        ssid: Vec<u8>,
        /// IEEE 802.11 channel number.
        channel: u8,
    },
}

impl fmt::Debug for WifiOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect { ssid, pmk, entropy } => formatter
                .debug_struct("Connect")
                .field("ssid", ssid)
                .field("pmk", pmk)
                .field("entropy", &entropy.as_ref().map(|_| "[REDACTED]"))
                .finish(),
            Self::Disconnect => formatter.write_str("Disconnect"),
            Self::StartOpenAccessPoint { ssid, channel } => formatter
                .debug_struct("StartOpenAccessPoint")
                .field("ssid", ssid)
                .field("channel", channel)
                .finish(),
        }
    }
}

impl Drop for WifiOperation {
    fn drop(&mut self) {
        if let Self::Connect {
            entropy: Some(entropy),
            ..
        } = self
        {
            entropy.zeroize();
        }
    }
}

/// One atomic wireless reconfiguration transaction.
///
/// The hardware operation and the protocol-side link policy are carried
/// together so the runtime can commit the IP/DHCP role only after the owner-CPU
/// hardware transition succeeds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WifiTransaction {
    operation: WifiOperation,
    link_policy: Option<WifiLinkPolicy>,
}

impl WifiTransaction {
    /// Creates an open station-mode transaction.
    pub fn connect_open(ssid: impl Into<String>) -> Self {
        Self {
            operation: WifiOperation::Connect {
                ssid: ssid.into(),
                pmk: None,
                entropy: None,
            },
            link_policy: None,
        }
    }

    /// Creates a WPA2 station transaction. Runtime-owned entropy is added at
    /// submission when the caller does not provide it explicitly.
    pub fn connect_wpa2_pmk(ssid: impl Into<String>, pmk: Wpa2Pmk) -> Self {
        Self {
            operation: WifiOperation::Connect {
                ssid: ssid.into(),
                pmk: Some(pmk),
                entropy: None,
            },
            link_policy: None,
        }
    }

    /// Creates a WPA2 station transaction with explicit caller-owned entropy.
    pub fn connect_wpa2_pmk_with_entropy(
        ssid: impl Into<String>,
        pmk: Wpa2Pmk,
        entropy: [u8; 32],
    ) -> Self {
        Self {
            operation: WifiOperation::Connect {
                ssid: ssid.into(),
                pmk: Some(pmk),
                entropy: Some(entropy),
            },
            link_policy: None,
        }
    }

    /// Returns whether this secured station transaction still needs entropy
    /// from the owning network runtime.
    pub fn needs_connect_entropy(&self) -> bool {
        matches!(
            &self.operation,
            WifiOperation::Connect {
                pmk: Some(_),
                entropy: None,
                ..
            }
        )
    }

    /// Installs runtime-owned entropy only when a secured connection does not
    /// already carry explicit caller entropy.
    pub fn provide_connect_entropy(&mut self, provided: [u8; 32]) {
        if let WifiOperation::Connect {
            pmk: Some(_),
            entropy,
            ..
        } = &mut self.operation
            && entropy.is_none()
        {
            *entropy = Some(provided);
        }
    }

    /// Creates a station disconnect transaction.
    pub const fn disconnect() -> Self {
        Self {
            operation: WifiOperation::Disconnect,
            link_policy: None,
        }
    }

    /// Creates an open-access-point transaction and its matching IP policy.
    pub fn open_access_point(
        ssid: impl Into<Vec<u8>>,
        channel: u8,
        link_policy: WifiLinkPolicy,
    ) -> Self {
        Self {
            operation: WifiOperation::StartOpenAccessPoint {
                ssid: ssid.into(),
                channel,
            },
            link_policy: Some(link_policy),
        }
    }

    /// Returns the hardware operation executed by the queue owner.
    pub const fn operation(&self) -> &WifiOperation {
        &self.operation
    }

    /// Returns the protocol policy committed after hardware success.
    pub const fn link_policy(&self) -> Option<WifiLinkPolicy> {
        self.link_policy
    }
}

/// Optional owned control plane for a wireless [`NetDevice`].
///
/// Bundles wireless-specific STA/SoftAP operations and link policy into an
/// owned endpoint returned alongside the queue parts. Wireless devices use the
/// same runtime lifecycle and IRQ topology as every other network device.
pub trait WifiControl: Send + 'static {
    /// Begins one hardware transition on the poll-group owner CPU.
    ///
    /// The runtime quiesces the poll group for this finite step and rearms it
    /// before waiting for the returned interrupt or deadline.
    fn start(
        &mut self,
        operation: &WifiOperation,
        now_nanos: u64,
    ) -> Result<WifiControlProgress, NetError>;

    /// Advances the active hardware transition by one finite owner step.
    fn advance(&mut self, now_nanos: u64) -> Result<WifiControlProgress, NetError>;

    /// Cancels the active transition and aborts any in-flight device request.
    fn cancel(&mut self) -> Result<(), NetError>;

    /// Returns the optional board-selected startup transaction. The network
    /// runtime executes it only after worker affinity and IRQ registration are
    /// ready, but before publishing the network service.
    fn startup_transaction(&self) -> Option<WifiTransaction>;
}

// ---------------------------------------------------------------------------
// Transmit queue
// ---------------------------------------------------------------------------

/// Hardware-notification policy for one transmit submission.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TxNotify {
    /// Make the submitted descriptor visible to the device immediately.
    #[default]
    Immediate,
    /// Defer notification until [`ITxQueue::flush`] is called.
    Deferred,
}

/// Per-packet options passed across the runtime transmit boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TxSubmitOptions {
    pub checksum: Option<TxChecksumOffload>,
    pub notify: TxNotify,
}

impl TxSubmitOptions {
    pub const fn immediate(checksum: Option<TxChecksumOffload>) -> Self {
        Self {
            checksum,
            notify: TxNotify::Immediate,
        }
    }

    pub const fn deferred(checksum: Option<TxChecksumOffload>) -> Self {
        Self {
            checksum,
            notify: TxNotify::Deferred,
        }
    }
}

/// Transmit queue interface.
///
/// A driver moves one TX queue into each [`NetPollGroupParts`].
pub trait ITxQueue: Send + 'static {
    /// Queue identifier (unique within the device).
    fn id(&self) -> NetQueueId;

    /// DMA buffer configuration for this queue.
    fn config(&self) -> QueueConfig;

    /// Returns transport checksums this queue can calculate.
    fn checksum_capabilities(&self) -> TxChecksumCapabilities {
        TxChecksumCapabilities::NONE
    }

    /// Submit a DMA buffer for transmission.
    ///
    /// `bus_addr` must point to a DMA-capable buffer whose first `len` bytes
    /// contain the packet to be transmitted. A [`NetError::Retry`] or
    /// [`NetError::LinkDown`] failure must have a future queue or link event
    /// that can wake the fixed-CPU poll owner after it rearms IRQs.
    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError>;

    /// Submits a buffer with checksum and device-notification options.
    ///
    /// The default rejects checksum requests and otherwise preserves the
    /// existing immediate-notification behavior. A rejected submission must
    /// return the original move-only token to the runtime.
    fn submit_with_options(
        &mut self,
        buffer: DmaBuffer,
        options: TxSubmitOptions,
    ) -> Result<(), SubmitError> {
        if options.checksum.is_some() {
            Err(SubmitError::new(buffer, NetError::NotSupported))
        } else {
            self.submit(buffer)
        }
    }

    /// Makes all deferred descriptors visible to the device.
    fn flush(&mut self) {}

    /// Reclaim the next completed transmit buffer.
    ///
    /// Returns the original move-only token after the device completes it.
    fn reclaim(&mut self) -> Option<DmaBuffer>;
}

// ---------------------------------------------------------------------------
// Receive queue
// ---------------------------------------------------------------------------

/// Receive queue interface.
///
/// A driver moves one RX queue into each [`NetPollGroupParts`].
pub trait IRxQueue: Send + 'static {
    /// Queue identifier (unique within the device).
    fn id(&self) -> NetQueueId;

    /// DMA buffer configuration for this queue.
    fn config(&self) -> QueueConfig;

    /// Submit an empty DMA buffer to hardware.
    ///
    /// `bus_addr` must point to a DMA-capable buffer whose total size is `len`.
    /// A [`NetError::Retry`] failure must have a future queue or task-side
    /// device event that can wake the fixed-CPU poll owner after it rearms IRQs.
    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), SubmitError>;

    /// Reclaim the next completed receive buffer.
    ///
    /// Returns the original token and the received byte count.
    fn reclaim(&mut self) -> Option<RxCompletion>;
}

/// One completed receive buffer returned by a hardware queue.
#[derive(Debug)]
pub struct RxCompletion {
    /// Original buffer token submitted to the queue.
    pub buffer: DmaBuffer,
    /// Number of received bytes at the beginning of `buffer`.
    pub packet_len: usize,
}
