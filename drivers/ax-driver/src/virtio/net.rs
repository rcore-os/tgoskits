extern crate alloc;

use alloc::{boxed::Box, collections::BTreeMap, format, string::ToString, sync::Arc};
use core::{fmt, mem::size_of, ptr::NonNull};

use ax_kspin::SpinNoIrq;
use rd_net::{
    ContainmentCause, DmaBuffer, EthernetIrqFault, Event, IRxQueue, ITxQueue, InterfaceIrqEndpoint,
    IrqCapture, MaskedSource, NetError, OwnerInitInput, OwnerInitPoll, OwnerInitSchedule,
    QueueConfig, QueueMemoryMode,
};
use rdrive::{DriverGeneric, PlatformDevice, probe::OnProbeError};
#[cfg(feature = "pci")]
use virtio_drivers::transport::DeviceType;
use virtio_drivers::{
    BufferDirection, Error as VirtIoError, Hal as VirtIoHal, PAGE_SIZE,
    queue::VirtQueue,
    transport::{DeviceStatus, InterruptStatus},
};

#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};
use crate::{
    net::PlatformDeviceNet,
    virtio::{self, VirtIoHalImpl, VirtIoTransport},
};

const QUEUE_SIZE: usize = 64;
const BUFFER_SIZE: usize = 2048;
const MIN_RECEIVE_BUFFER_SIZE: usize = 1526;
const RECEIVE_QUEUE: u16 = 0;
const TRANSMIT_QUEUE: u16 = 1;
const LEGACY_HEADER_SIZE: usize = 10;
const MODERN_HEADER_SIZE: usize = 12;

const VIRTIO_NET_F_MAC: u64 = 1 << 5;
const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;
const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
const VIRTIO_F_RING_INDIRECT_DESC: u64 = 1 << 28;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const SUPPORTED_FEATURES: u64 =
    VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS | VIRTIO_F_RING_INDIRECT_DESC | VIRTIO_F_VERSION_1;

const RESET_RETRY_NS: u64 = 50_000;
const INITIALIZATION_TIMEOUT_NS: u64 = 1_000_000_000;
const CONFIG_GENERATION_RETRY_LIMIT: u8 = 64;

const MMIO_INTERRUPT_STATUS_OFFSET: usize = 0x60;
const MMIO_INTERRUPT_ACK_OFFSET: usize = 0x64;
const MMIO_INTERRUPT_REGISTERS_END: usize = MMIO_INTERRUPT_ACK_OFFSET + size_of::<u32>();

#[cfg(feature = "pci")]
const PCI_VIRTIO_ISR_CONFIG_TYPE: u8 = 3;
#[cfg(feature = "pci")]
const PCI_VIRTIO_ISR_CAP_MIN_LENGTH: u8 = 16;
#[cfg(feature = "pci")]
const PCI_CAP_BAR_OFFSET: u16 = 4;
#[cfg(feature = "pci")]
const PCI_CAP_REGION_OFFSET: u16 = 8;
#[cfg(feature = "pci")]
const PCI_CAP_REGION_LENGTH: u16 = 12;

#[cfg(feature = "pci")]
crate::model_register!(
    name: "VirtIO Net",
    level: ProbeLevel::PostKernel,
    priority: ProbePriority::DEFAULT,
    probe_kinds: &[ProbeKind::Pci {
        on_probe: probe_pci,
    }],
);

struct VirtIoNetDevice<T: VirtIoTransport> {
    owner: VirtioNetOwnerState<T>,
    interrupt_port: Option<VirtioNetInterruptPort>,
    irq_enabled: bool,
    mac: [u8; 6],
    tx_created: bool,
    rx_created: bool,
}

impl<T: VirtIoTransport> VirtIoNetDevice<T> {
    fn new(
        transport: T,
        interrupt_port: VirtioNetInterruptPort,
        transport_mapping: Option<Arc<mmio_api::Mmio>>,
    ) -> Self {
        Self {
            owner: VirtioNetOwnerState::Pending(VirtioNetPending {
                transport,
                _transport_mapping: transport_mapping,
            }),
            interrupt_port: Some(interrupt_port),
            irq_enabled: false,
            mac: [0; 6],
            tx_created: false,
            rx_created: false,
        }
    }

    fn ready(&self) -> Option<&Arc<SpinNoIrq<NetInner<T>>>> {
        match &self.owner {
            VirtioNetOwnerState::Ready(ready) => Some(ready),
            VirtioNetOwnerState::Pending(_)
            | VirtioNetOwnerState::Initializing(_)
            | VirtioNetOwnerState::Transitioning => None,
        }
    }

    fn poll_owner_initialization(&mut self, now_ns: u64) -> OwnerInitPoll {
        let state = core::mem::replace(&mut self.owner, VirtioNetOwnerState::Transitioning);
        let mut initialization = match state {
            VirtioNetOwnerState::Pending(pending) => Box::new(pending.begin(now_ns)),
            VirtioNetOwnerState::Initializing(initialization) => initialization,
            VirtioNetOwnerState::Ready(ready) => {
                self.owner = VirtioNetOwnerState::Ready(ready);
                return OwnerInitPoll::Ready;
            }
            VirtioNetOwnerState::Transitioning => {
                return OwnerInitPoll::Failed(initialization_error(
                    VirtioNetInitError::InvalidState,
                ));
            }
        };

        match initialization.poll_owner_initialization(now_ns) {
            Ok(VirtioNetInitProgress::Pending(schedule)) => {
                self.owner = VirtioNetOwnerState::Initializing(initialization);
                OwnerInitPoll::Pending(schedule)
            }
            Ok(VirtioNetInitProgress::Ready) => match initialization.take_ready() {
                Ok(raw) => {
                    self.mac = raw.mac_address();
                    self.owner =
                        VirtioNetOwnerState::Ready(Arc::new(SpinNoIrq::new(NetInner::new(raw))));
                    OwnerInitPoll::Ready
                }
                Err(error) => {
                    self.owner = VirtioNetOwnerState::Initializing(initialization);
                    OwnerInitPoll::Failed(initialization_error(error))
                }
            },
            Err(error) => {
                // Keep the transaction intact. Runtime failure handling owns
                // the decision to close it or quarantine it; either path keeps
                // transport and queue DMA ownership explicit.
                self.owner = VirtioNetOwnerState::Initializing(initialization);
                OwnerInitPoll::Failed(initialization_error(error))
            }
        }
    }
}

impl<T: VirtIoTransport> DriverGeneric for VirtIoNetDevice<T> {
    fn name(&self) -> &str {
        "virtio-net"
    }
}

impl<T: VirtIoTransport> rd_net::Interface for VirtIoNetDevice<T> {
    fn poll_owner_init(&mut self, input: OwnerInitInput) -> OwnerInitPoll {
        self.poll_owner_initialization(input.now_ns)
    }

    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn create_tx_queue(&mut self) -> Option<Box<dyn ITxQueue>> {
        if self.tx_created {
            return None;
        }
        let ready = Arc::clone(self.ready()?);
        self.tx_created = true;
        Some(Box::new(NetTxQueue { inner: ready }))
    }

    fn create_rx_queue(&mut self) -> Option<Box<dyn IRxQueue>> {
        if self.rx_created {
            return None;
        }
        let ready = Arc::clone(self.ready()?);
        self.rx_created = true;
        Some(Box::new(NetRxQueue { inner: ready }))
    }

    fn enable_irq(&mut self) -> Result<(), NetError> {
        let ready = self.ready().ok_or_else(net_not_ready)?;
        ready.lock().raw.enable_interrupts();
        self.irq_enabled = true;
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<(), NetError> {
        let ready = self.ready().ok_or_else(net_not_ready)?;
        ready.lock().raw.disable_interrupts();
        self.irq_enabled = false;
        Ok(())
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    fn take_irq_endpoint(&mut self) -> Option<rd_net::BIrqEndpoint> {
        self.interrupt_port
            .take()
            .map(|port| Box::new(VirtioNetIrqEndpoint { port }) as rd_net::BIrqEndpoint)
    }

    fn service_irq_event(&mut self, event: Event) -> Result<(), NetError> {
        let ready = self.ready().ok_or_else(net_not_ready)?;
        let mut ready = ready.lock();
        if event.tx_queue.contains(0) {
            ready.open_tx_irq_continuation();
        }
        if event.rx_queue.contains(0) {
            ready.open_rx_irq_continuation();
        }
        Ok(())
    }
}

/// Destructive VirtIO interrupt-status capability independent of the queue
/// transport used by the maintenance owner.
pub struct VirtioNetInterruptPort {
    registers: VirtioNetInterruptRegisters,
}

impl VirtioNetInterruptPort {
    pub fn from_mmio(mapping: mmio_api::Mmio) -> Result<Self, NetError> {
        Self::from_shared_mmio(Arc::new(mapping))
    }

    fn from_shared_mmio(mapping: Arc<mmio_api::Mmio>) -> Result<Self, NetError> {
        if mapping.size() < MMIO_INTERRUPT_REGISTERS_END {
            return Err(NetError::Other(Box::new(rd_net::KError::Unknown(
                "virtio MMIO mapping does not contain interrupt registers",
            ))));
        }
        Ok(Self {
            registers: VirtioNetInterruptRegisters::Mmio(mapping),
        })
    }

    pub fn from_pci_isr(mapping: mmio_api::Mmio) -> Result<Self, NetError> {
        if mapping.size() < size_of::<u8>() {
            return Err(NetError::Other(Box::new(rd_net::KError::Unknown(
                "virtio PCI ISR mapping is empty",
            ))));
        }
        Ok(Self {
            registers: VirtioNetInterruptRegisters::Pci(mapping),
        })
    }

    fn capture_status(&mut self) -> u32 {
        match &self.registers {
            VirtioNetInterruptRegisters::Mmio(mapping) => {
                let status = mapping.read::<u32>(MMIO_INTERRUPT_STATUS_OFFSET);
                if status != 0 {
                    mapping.write(MMIO_INTERRUPT_ACK_OFFSET, status);
                }
                status
            }
            // VirtIO PCI defines the ISR read itself as acknowledgement.
            VirtioNetInterruptRegisters::Pci(mapping) => u32::from(mapping.read::<u8>(0)),
        }
    }
}

enum VirtioNetInterruptRegisters {
    Mmio(Arc<mmio_api::Mmio>),
    Pci(mmio_api::Mmio),
}

struct VirtioNetIrqEndpoint {
    port: VirtioNetInterruptPort,
}

impl InterfaceIrqEndpoint for VirtioNetIrqEndpoint {
    type Event = Event;
    type Fault = EthernetIrqFault;

    fn capture(&mut self) -> IrqCapture<Event, EthernetIrqFault> {
        let raw = self.port.capture_status();
        if raw == 0 {
            return IrqCapture::Unhandled;
        }
        let status = InterruptStatus::from_bits_truncate(raw);
        let mut event = Event::none();
        event.device_status = u64::from(raw);
        if status.contains(InterruptStatus::QUEUE_INTERRUPT) {
            event.tx_queue.insert(0);
            event.rx_queue.insert(0);
        }
        IrqCapture::Captured {
            event,
            masked: None,
        }
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, EthernetIrqFault> {
        // Neither VirtIO MMIO nor the portable PCI ISR capability exposes an
        // exact device-side source mask. Runtime glue must mask this action.
        Err(EthernetIrqFault::Containment)
    }
}

struct VirtioNetPending<T: VirtIoTransport> {
    transport: T,
    _transport_mapping: Option<Arc<mmio_api::Mmio>>,
}

impl<T: VirtIoTransport> VirtioNetPending<T> {
    fn begin(self, now_ns: u64) -> VirtioNetInitialization<T> {
        VirtioNetInitialization {
            receive_queue: None,
            send_queue: None,
            transport: Some(self.transport),
            _transport_mapping: self._transport_mapping,
            stage: VirtioNetInitStage::Reset,
            negotiated_features: 0,
            mac: [0; 6],
            config_generation_retries: 0,
            initialization_deadline_ns: now_ns.saturating_add(INITIALIZATION_TIMEOUT_NS),
        }
    }
}

enum VirtioNetOwnerState<T: VirtIoTransport> {
    Pending(VirtioNetPending<T>),
    Initializing(Box<VirtioNetInitialization<T>>),
    Ready(Arc<SpinNoIrq<NetInner<T>>>),
    Transitioning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VirtioNetInitStage {
    Reset,
    WaitForReset,
    NegotiateFeatures,
    ReadConfiguration,
    CreateReceiveQueue,
    CreateTransmitQueue,
    Finish,
    Finished,
}

struct VirtioNetInitialization<T: VirtIoTransport> {
    // Queue DMA must drop before transport and its backing mapping. Drop also
    // unsets every published queue while the transport is still live.
    receive_queue: Option<VirtQueue<VirtIoHalImpl, QUEUE_SIZE>>,
    send_queue: Option<VirtQueue<VirtIoHalImpl, QUEUE_SIZE>>,
    transport: Option<T>,
    _transport_mapping: Option<Arc<mmio_api::Mmio>>,
    stage: VirtioNetInitStage,
    negotiated_features: u64,
    mac: [u8; 6],
    config_generation_retries: u8,
    initialization_deadline_ns: u64,
}

// SAFETY: the transaction and all partially constructed queue DMA move as one
// value into the fixed maintenance owner. No references escape a poll call.
unsafe impl<T: VirtIoTransport> Send for VirtioNetInitialization<T> {}

impl<T: VirtIoTransport> VirtioNetInitialization<T> {
    fn transport(&mut self) -> Result<&mut T, VirtioNetInitError> {
        self.transport
            .as_mut()
            .ok_or(VirtioNetInitError::InvalidState)
    }

    fn poll_owner_initialization(
        &mut self,
        now_ns: u64,
    ) -> Result<VirtioNetInitProgress, VirtioNetInitError> {
        if now_ns >= self.initialization_deadline_ns {
            return Err(VirtioNetInitError::Timeout);
        }

        match self.stage {
            VirtioNetInitStage::Reset => {
                self.transport()?.set_status(DeviceStatus::empty());
                self.stage = VirtioNetInitStage::WaitForReset;
                Ok(VirtioNetInitProgress::Pending(
                    OwnerInitSchedule::wait_until(
                        now_ns
                            .saturating_add(RESET_RETRY_NS)
                            .min(self.initialization_deadline_ns),
                    ),
                ))
            }
            VirtioNetInitStage::WaitForReset => {
                if !self.transport()?.get_status().is_empty() {
                    return Ok(VirtioNetInitProgress::Pending(
                        OwnerInitSchedule::wait_until(
                            now_ns
                                .saturating_add(RESET_RETRY_NS)
                                .min(self.initialization_deadline_ns),
                        ),
                    ));
                }
                self.stage = VirtioNetInitStage::NegotiateFeatures;
                Ok(VirtioNetInitProgress::Pending(
                    OwnerInitSchedule::run_again(),
                ))
            }
            VirtioNetInitStage::NegotiateFeatures => {
                let transport = self.transport()?;
                let base_status = DeviceStatus::ACKNOWLEDGE | DeviceStatus::DRIVER;
                transport.set_status(base_status);
                let offered = transport.read_device_features();
                let negotiated = offered & SUPPORTED_FEATURES;
                transport.write_driver_features(negotiated);
                transport.set_status(base_status | DeviceStatus::FEATURES_OK);
                if !transport.get_status().contains(DeviceStatus::FEATURES_OK) {
                    return Err(VirtioNetInitError::FeatureNegotiationRejected);
                }
                transport.set_guest_page_size(PAGE_SIZE as u32);
                self.negotiated_features = negotiated;
                self.stage = VirtioNetInitStage::ReadConfiguration;
                Ok(VirtioNetInitProgress::Pending(
                    OwnerInitSchedule::run_again(),
                ))
            }
            VirtioNetInitStage::ReadConfiguration => {
                // One generation transaction per activation. Unlike upstream
                // read_consistent(), this cannot spin forever if a device keeps
                // changing its configuration generation.
                let transport = self.transport()?;
                let before = transport.read_config_generation();
                let mac = transport
                    .read_config_space::<[u8; 6]>(0)
                    .map_err(VirtioNetInitError::Device)?;
                let after = transport.read_config_generation();
                if before != after {
                    self.config_generation_retries =
                        self.config_generation_retries.saturating_add(1);
                    if self.config_generation_retries >= CONFIG_GENERATION_RETRY_LIMIT {
                        return Err(VirtioNetInitError::ConfigurationChurn);
                    }
                    return Ok(VirtioNetInitProgress::Pending(
                        OwnerInitSchedule::run_again(),
                    ));
                }
                self.mac = mac;
                self.stage = VirtioNetInitStage::CreateReceiveQueue;
                Ok(VirtioNetInitProgress::Pending(
                    OwnerInitSchedule::run_again(),
                ))
            }
            VirtioNetInitStage::CreateReceiveQueue => {
                let indirect = self.negotiated_features & VIRTIO_F_RING_INDIRECT_DESC != 0;
                // VirtQueue::new performs a fixed QUEUE_SIZE - 1 descriptor
                // initialization; QUEUE_SIZE is the compile-time constant 64.
                // EVENT_IDX is intentionally not negotiated: virtio-drivers'
                // set_dev_notify() cannot suppress used notifications in that
                // mode, so it would violate the runtime's source-mask proof.
                let queue = VirtQueue::new(self.transport()?, RECEIVE_QUEUE, indirect, false)
                    .map_err(VirtioNetInitError::Device)?;
                self.receive_queue = Some(queue);
                self.stage = VirtioNetInitStage::CreateTransmitQueue;
                Ok(VirtioNetInitProgress::Pending(
                    OwnerInitSchedule::run_again(),
                ))
            }
            VirtioNetInitStage::CreateTransmitQueue => {
                let indirect = self.negotiated_features & VIRTIO_F_RING_INDIRECT_DESC != 0;
                let queue = VirtQueue::new(self.transport()?, TRANSMIT_QUEUE, indirect, false)
                    .map_err(VirtioNetInitError::Device)?;
                self.send_queue = Some(queue);
                self.stage = VirtioNetInitStage::Finish;
                Ok(VirtioNetInitProgress::Pending(
                    OwnerInitSchedule::run_again(),
                ))
            }
            VirtioNetInitStage::Finish => {
                self.transport()?.finish_init();
                self.receive_queue
                    .as_mut()
                    .ok_or(VirtioNetInitError::InvalidState)?
                    .set_dev_notify(false);
                self.send_queue
                    .as_mut()
                    .ok_or(VirtioNetInitError::InvalidState)?
                    .set_dev_notify(false);
                self.stage = VirtioNetInitStage::Finished;
                Ok(VirtioNetInitProgress::Ready)
            }
            VirtioNetInitStage::Finished => Err(VirtioNetInitError::InvalidState),
        }
    }

    fn take_ready(&mut self) -> Result<OwnerVirtioNetRaw<T>, VirtioNetInitError> {
        if self.stage != VirtioNetInitStage::Finished
            || self.receive_queue.is_none()
            || self.send_queue.is_none()
            || self.transport.is_none()
        {
            return Err(VirtioNetInitError::InvalidState);
        }

        // All three owners were validated together while `&mut self` excludes
        // mutation. From this point construction is infallible, so no `?`
        // can drop one queue while the live transport still advertises it.
        let receive_queue = self
            .receive_queue
            .take()
            .expect("validated receive queue disappeared");
        let send_queue = self
            .send_queue
            .take()
            .expect("validated transmit queue disappeared");
        let transport = self
            .transport
            .take()
            .expect("validated transport disappeared");
        Ok(OwnerVirtioNetRaw {
            receive_queue,
            send_queue,
            transport,
            _transport_mapping: self._transport_mapping.take(),
            mac: self.mac,
            legacy_header: self.negotiated_features & VIRTIO_F_VERSION_1 == 0
                && self.negotiated_features & VIRTIO_NET_F_MRG_RXBUF == 0,
        })
    }
}

impl<T: VirtIoTransport> Drop for VirtioNetInitialization<T> {
    fn drop(&mut self) {
        let Some(transport) = self.transport.as_mut() else {
            return;
        };
        if self.send_queue.is_some() {
            transport.queue_unset(TRANSMIT_QUEUE);
        }
        if self.receive_queue.is_some() {
            transport.queue_unset(RECEIVE_QUEUE);
        }
    }
}

enum VirtioNetInitProgress {
    Pending(OwnerInitSchedule),
    Ready,
}

#[derive(Debug)]
enum VirtioNetInitError {
    Device(VirtIoError),
    Timeout,
    ConfigurationChurn,
    FeatureNegotiationRejected,
    InvalidState,
}

impl fmt::Display for VirtioNetInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(error) => write!(f, "VirtIO network initialization failed: {error:?}"),
            Self::Timeout => write!(f, "VirtIO network initialization timed out"),
            Self::ConfigurationChurn => {
                write!(
                    f,
                    "VirtIO network configuration generation never stabilized"
                )
            }
            Self::FeatureNegotiationRejected => {
                write!(f, "VirtIO network feature negotiation was rejected")
            }
            Self::InvalidState => write!(f, "VirtIO network initialization state is invalid"),
        }
    }
}

impl core::error::Error for VirtioNetInitError {}

fn initialization_error(error: VirtioNetInitError) -> NetError {
    NetError::Other(Box::new(error))
}

struct OwnerVirtioNetRaw<T: VirtIoTransport> {
    // Drop order is queue DMA, transport, then the mapping used by transport.
    receive_queue: VirtQueue<VirtIoHalImpl, QUEUE_SIZE>,
    send_queue: VirtQueue<VirtIoHalImpl, QUEUE_SIZE>,
    transport: T,
    _transport_mapping: Option<Arc<mmio_api::Mmio>>,
    mac: [u8; 6],
    legacy_header: bool,
}

impl<T: VirtIoTransport> OwnerVirtioNetRaw<T> {
    fn mac_address(&self) -> [u8; 6] {
        self.mac
    }

    fn disable_interrupts(&mut self) {
        self.send_queue.set_dev_notify(false);
        self.receive_queue.set_dev_notify(false);
    }

    fn enable_interrupts(&mut self) {
        self.send_queue.set_dev_notify(true);
        self.receive_queue.set_dev_notify(true);
    }

    fn header_len(&self) -> usize {
        if self.legacy_header {
            LEGACY_HEADER_SIZE
        } else {
            MODERN_HEADER_SIZE
        }
    }

    fn fill_buffer_header(&self, buffer: &mut [u8]) -> Result<usize, VirtIoError> {
        let header_len = self.header_len();
        if buffer.len() < header_len {
            return Err(VirtIoError::InvalidParam);
        }
        buffer[..header_len].fill(0);
        Ok(header_len)
    }

    unsafe fn transmit_begin(&mut self, buffer: &[u8]) -> Result<u16, VirtIoError> {
        if buffer.len() < self.header_len() {
            return Err(VirtIoError::InvalidParam);
        }
        let token = unsafe { self.send_queue.add(&[buffer], &mut [])? };
        if self.send_queue.should_notify() {
            self.transport.notify(TRANSMIT_QUEUE);
        }
        Ok(token)
    }

    fn peek_transmit_used(&self) -> Option<u16> {
        self.send_queue.peek_used()
    }

    unsafe fn transmit_complete(&mut self, token: u16, buffer: &[u8]) -> Result<(), VirtIoError> {
        unsafe { self.send_queue.pop_used(token, &[buffer], &mut [])? };
        Ok(())
    }

    unsafe fn receive_begin(&mut self, buffer: &mut [u8]) -> Result<u16, VirtIoError> {
        if buffer.len() < MIN_RECEIVE_BUFFER_SIZE {
            return Err(VirtIoError::InvalidParam);
        }
        let token = unsafe { self.receive_queue.add(&[], &mut [buffer])? };
        if self.receive_queue.should_notify() {
            self.transport.notify(RECEIVE_QUEUE);
        }
        Ok(token)
    }

    fn peek_receive_used(&self) -> Option<u16> {
        self.receive_queue.peek_used()
    }

    unsafe fn receive_complete(
        &mut self,
        token: u16,
        buffer: &mut [u8],
    ) -> Result<(usize, usize), VirtIoError> {
        let used = unsafe { self.receive_queue.pop_used(token, &[], &mut [buffer])? } as usize;
        let header_len = self.header_len();
        let packet_len = used.checked_sub(header_len).ok_or(VirtIoError::IoError)?;
        Ok((header_len, packet_len))
    }
}

impl<T: VirtIoTransport> Drop for OwnerVirtioNetRaw<T> {
    fn drop(&mut self) {
        self.transport.queue_unset(RECEIVE_QUEUE);
        self.transport.queue_unset(TRANSMIT_QUEUE);
    }
}

struct NetInner<T: VirtIoTransport> {
    // Raw drops/unsets queue DMA before the inflight maps release buffers that
    // may still be referenced by descriptors.
    raw: OwnerVirtioNetRaw<T>,
    tx_inflight: BTreeMap<u16, TxInflight>,
    rx_inflight: BTreeMap<u16, RxInflight>,
    tx_irq_continuation: bool,
    rx_irq_continuation: bool,
}

// SAFETY: the complete owner raw, transport and DMA bookkeeping move together.
// Runtime invokes every method from the one fixed maintenance owner.
unsafe impl<T: VirtIoTransport> Send for NetInner<T> {}

impl<T: VirtIoTransport> NetInner<T> {
    fn new(raw: OwnerVirtioNetRaw<T>) -> Self {
        Self {
            raw,
            tx_inflight: BTreeMap::new(),
            rx_inflight: BTreeMap::new(),
            tx_irq_continuation: false,
            rx_irq_continuation: false,
        }
    }

    fn queue_config() -> QueueConfig {
        QueueConfig {
            dma_mask: u64::MAX,
            align: 0x1000,
            buf_size: BUFFER_SIZE,
            ring_size: QUEUE_SIZE,
            memory_mode: QueueMemoryMode::OwnerCopy,
        }
    }

    fn open_tx_irq_continuation(&mut self) {
        self.tx_irq_continuation = true;
    }

    fn open_rx_irq_continuation(&mut self) {
        self.rx_irq_continuation = true;
    }

    fn submit_tx(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if buffer.len == 0 || buffer.len > BUFFER_SIZE {
            return Err(NetError::NotSupported);
        }
        let packet = unsafe { core::slice::from_raw_parts(buffer.virt.as_ptr(), buffer.len) };
        let mut staging = VirtioPacketDma::new(
            self.raw.header_len() + buffer.len,
            BufferDirection::DriverToDevice,
        )?;
        let header_len = self
            .raw
            .fill_buffer_header(staging.as_mut_slice())
            .map_err(map_net_error)?;
        staging.as_mut_slice()[header_len..header_len + buffer.len].copy_from_slice(packet);
        let token =
            unsafe { self.raw.transmit_begin(staging.as_slice()) }.map_err(map_net_error)?;
        self.tx_inflight.insert(
            token,
            TxInflight {
                bus_addr: buffer.bus_addr,
                staging,
            },
        );
        Ok(())
    }

    fn reclaim_tx(&mut self) -> Option<u64> {
        if !self.tx_irq_continuation {
            return None;
        }
        let Some(token) = self.raw.peek_transmit_used() else {
            self.tx_irq_continuation = false;
            return None;
        };
        let Some(inflight) = self.tx_inflight.remove(&token) else {
            self.tx_irq_continuation = false;
            return None;
        };
        if unsafe {
            self.raw
                .transmit_complete(token, inflight.staging.as_slice())
        }
        .is_err()
        {
            self.tx_inflight.insert(token, inflight);
            self.tx_irq_continuation = false;
            return None;
        }
        Some(inflight.bus_addr)
    }

    fn submit_rx(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        if buffer.len < MIN_RECEIVE_BUFFER_SIZE {
            return Err(NetError::NotSupported);
        }
        let staging_len = buffer.len.min(BUFFER_SIZE);
        let mut staging = VirtioPacketDma::new(staging_len, BufferDirection::DeviceToDriver)?;
        let token =
            unsafe { self.raw.receive_begin(staging.as_mut_slice()) }.map_err(map_net_error)?;
        self.rx_inflight.insert(
            token,
            RxInflight {
                virt_addr: buffer.virt.as_ptr() as usize,
                bus_addr: buffer.bus_addr,
                len: buffer.len,
                staging,
            },
        );
        Ok(())
    }

    fn reclaim_rx(&mut self) -> Option<(u64, usize)> {
        if !self.rx_irq_continuation {
            return None;
        }
        let Some(token) = self.raw.peek_receive_used() else {
            self.rx_irq_continuation = false;
            return None;
        };
        let Some(mut inflight) = self.rx_inflight.remove(&token) else {
            self.rx_irq_continuation = false;
            return None;
        };
        let Ok((header_len, packet_len)) = (unsafe {
            self.raw
                .receive_complete(token, inflight.staging.as_mut_slice())
        }) else {
            self.rx_inflight.insert(token, inflight);
            self.rx_irq_continuation = false;
            return None;
        };
        let Some(payload_end) = header_len.checked_add(packet_len) else {
            return Some((inflight.bus_addr, 0));
        };
        let Some(payload) = inflight.staging.as_slice().get(header_len..payload_end) else {
            return Some((inflight.bus_addr, 0));
        };
        if payload.len() > inflight.len {
            return Some((inflight.bus_addr, 0));
        }
        // SAFETY: rd-net retains the submitted upper buffer until this exact
        // bus-address token is reclaimed. The bounds check above proves the
        // payload fits that allocation, and only the maintenance owner can
        // access this inflight entry.
        unsafe {
            core::ptr::copy_nonoverlapping(
                payload.as_ptr(),
                inflight.virt_addr as *mut u8,
                payload.len(),
            );
        }
        Some((inflight.bus_addr, packet_len))
    }
}

struct NetTxQueue<T: VirtIoTransport> {
    inner: Arc<SpinNoIrq<NetInner<T>>>,
}

impl<T: VirtIoTransport> ITxQueue for NetTxQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        NetInner::<T>::queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.inner.lock().submit_tx(buffer)
    }

    fn reclaim(&mut self) -> Option<u64> {
        self.inner.lock().reclaim_tx()
    }
}

struct NetRxQueue<T: VirtIoTransport> {
    inner: Arc<SpinNoIrq<NetInner<T>>>,
}

impl<T: VirtIoTransport> IRxQueue for NetRxQueue<T> {
    fn id(&self) -> usize {
        0
    }

    fn config(&self) -> QueueConfig {
        NetInner::<T>::queue_config()
    }

    fn submit(&mut self, buffer: DmaBuffer) -> Result<(), NetError> {
        self.inner.lock().submit_rx(buffer)
    }

    fn reclaim(&mut self) -> Option<(u64, usize)> {
        self.inner.lock().reclaim_rx()
    }
}

struct TxInflight {
    bus_addr: u64,
    staging: VirtioPacketDma,
}

struct RxInflight {
    virt_addr: usize,
    bus_addr: u64,
    len: usize,
    staging: VirtioPacketDma,
}

struct VirtioPacketDma {
    physical_address: u64,
    virtual_address: NonNull<u8>,
    pages: usize,
    len: usize,
}

impl VirtioPacketDma {
    fn new(len: usize, direction: BufferDirection) -> Result<Self, NetError> {
        if len == 0 {
            return Err(NetError::NotSupported);
        }
        let pages = len.div_ceil(PAGE_SIZE);
        let (physical_address, virtual_address) =
            <VirtIoHalImpl as VirtIoHal>::dma_alloc(pages, direction);
        if physical_address == 0 {
            return Err(NetError::NoMemory);
        }
        Ok(Self {
            physical_address,
            virtual_address,
            pages,
            len,
        })
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: `virtual_address` owns `pages * PAGE_SIZE` bytes until Drop;
        // `len` was used to size that allocation and immutable access is
        // confined to the maintenance owner.
        unsafe { core::slice::from_raw_parts(self.virtual_address.as_ptr(), self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the unique borrow excludes every other CPU-side access and
        // `len` is within the live page allocation.
        unsafe { core::slice::from_raw_parts_mut(self.virtual_address.as_ptr(), self.len) }
    }
}

impl Drop for VirtioPacketDma {
    fn drop(&mut self) {
        // SAFETY: these are the unchanged values returned by `dma_alloc`, and
        // the staging allocation is dropped only after VirtQueue::pop_used has
        // unshared the descriptor or before it was ever submitted.
        let result = unsafe {
            <VirtIoHalImpl as VirtIoHal>::dma_dealloc(
                self.physical_address,
                self.virtual_address,
                self.pages,
            )
        };
        debug_assert_eq!(result, 0, "failed to release VirtIO packet DMA staging");
    }
}

#[cfg(feature = "pci")]
fn probe_pci(mut probe: rdrive::probe::pci::ProbePci<'_>) -> Result<(), OnProbeError> {
    crate::pci::ensure_virtio_pci_endpoint(probe.endpoint(), DeviceType::Network)?;
    let interrupt_port = pci_interrupt_port(probe.endpoint())?;
    let transport = crate::pci::take_virtio_transport(probe.endpoint_mut(), DeviceType::Network)?;
    register_pci_transport(probe, transport, interrupt_port)
}

pub fn register_transport<T: VirtIoTransport>(
    _plat_dev: PlatformDevice,
    _transport: T,
) -> Result<(), OnProbeError> {
    Err(OnProbeError::other(
        "virtio network registration requires an independent interrupt port",
    ))
}

pub fn register_transport_with_interrupt_port<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioNetInterruptPort,
) -> Result<(), OnProbeError> {
    register_net(plat_dev, transport, interrupt_port, None)
}

pub fn register_mmio_transport<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    mapping: mmio_api::Mmio,
) -> Result<(), OnProbeError> {
    register_owned_mmio(plat_dev, transport, mapping)
}

/// Registers a transport whose MMIO pointers are backed by this exact RAII
/// mapping. The owner and detached IRQ endpoint retain shared leases, so either
/// side can close independently without leaving a dangling register pointer.
pub fn register_owned_mmio<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    mapping: mmio_api::Mmio,
) -> Result<(), OnProbeError> {
    let mapping = Arc::new(mapping);
    let interrupt_port = VirtioNetInterruptPort::from_shared_mmio(Arc::clone(&mapping))
        .map_err(|error| OnProbeError::other(error.to_string()))?;
    register_net(plat_dev, transport, interrupt_port, Some(mapping))
}

#[cfg(feature = "pci")]
fn register_pci_transport<T: VirtIoTransport>(
    probe: rdrive::probe::pci::ProbePci<'_>,
    transport: T,
    interrupt_port: VirtioNetInterruptPort,
) -> Result<(), OnProbeError> {
    let info = binding_info_from_pci(probe.info(), PciIrqRequirement::Required)?;
    let net = make_net(transport, interrupt_port, None);
    let irq = probe
        .into_platform_device()
        .register_net_with_info("virtio-net", net, info);
    log::info!("registered virtio network device irq={irq:?}");
    Ok(())
}

fn register_net<T: VirtIoTransport>(
    plat_dev: PlatformDevice,
    transport: T,
    interrupt_port: VirtioNetInterruptPort,
    transport_mapping: Option<Arc<mmio_api::Mmio>>,
) -> Result<(), OnProbeError> {
    let net = make_net(transport, interrupt_port, transport_mapping);
    let irq = plat_dev.register_net("virtio-net", net);
    log::info!("registered virtio network device irq={irq:?}");
    Ok(())
}

fn make_net<T: VirtIoTransport>(
    transport: T,
    interrupt_port: VirtioNetInterruptPort,
    transport_mapping: Option<Arc<mmio_api::Mmio>>,
) -> VirtIoNetDevice<T> {
    VirtIoNetDevice::new(transport, interrupt_port, transport_mapping)
}

#[cfg(feature = "pci")]
fn pci_interrupt_port(
    endpoint: &rdrive::probe::pci::Endpoint,
) -> Result<VirtioNetInterruptPort, OnProbeError> {
    use rdrive::probe::pci::PciCapability;

    for capability in endpoint.capabilities() {
        let PciCapability::Vendor(address) = capability else {
            continue;
        };
        let header = endpoint.read(address.offset);
        if (header >> 24) as u8 != PCI_VIRTIO_ISR_CONFIG_TYPE {
            continue;
        }
        let capability_length = (header >> 16) as u8;
        if capability_length < PCI_VIRTIO_ISR_CAP_MIN_LENGTH {
            return Err(OnProbeError::other(
                "virtio PCI ISR capability is shorter than its fixed fields",
            ));
        }
        let bar = endpoint.read(address.offset + PCI_CAP_BAR_OFFSET) as u8;
        if bar >= 6 {
            return Err(OnProbeError::other(format!(
                "virtio PCI ISR capability names invalid BAR {bar}"
            )));
        }
        let region_offset = endpoint.read(address.offset + PCI_CAP_REGION_OFFSET) as usize;
        let region_length = endpoint.read(address.offset + PCI_CAP_REGION_LENGTH) as usize;
        if region_length == 0 {
            return Err(OnProbeError::other(
                "virtio PCI ISR capability has zero length",
            ));
        }
        let bar_range = endpoint.bar_mmio(bar).ok_or_else(|| {
            OnProbeError::other(format!("virtio PCI ISR capability names invalid BAR {bar}"))
        })?;
        let isr_phys = bar_range
            .start
            .checked_add(region_offset)
            .filter(|start| {
                start
                    .checked_add(region_length)
                    .is_some_and(|end| end <= bar_range.end)
            })
            .ok_or_else(|| OnProbeError::other("virtio PCI ISR capability exceeds its BAR"))?;
        let mapping = axklib::mmio::ioremap(isr_phys.into(), region_length)
            .map_err(|error| OnProbeError::other(format!("{error:?}")))?;
        return VirtioNetInterruptPort::from_pci_isr(mapping)
            .map_err(|error| OnProbeError::other(error.to_string()));
    }
    Err(OnProbeError::other(
        "virtio PCI transport has no ISR capability",
    ))
}

fn net_not_ready() -> NetError {
    NetError::Other(Box::new(rd_net::KError::Unknown(
        "VirtIO network owner is not ready",
    )))
}

fn map_net_error(error: VirtIoError) -> NetError {
    match error {
        VirtIoError::QueueFull | VirtIoError::NotReady => NetError::Retry,
        VirtIoError::DmaError => NetError::NoMemory,
        VirtIoError::Unsupported => NetError::NotSupported,
        other => NetError::Other(Box::new(rd_net::KError::Unknown(virtio::map_virtio_error(
            other,
        )))),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::sync::Arc;
    use core::{
        cell::Cell,
        mem::{MaybeUninit, size_of},
        sync::atomic::{AtomicU64, AtomicUsize, Ordering},
    };

    use virtio_drivers::{
        PhysAddr,
        transport::{DeviceType, InterruptStatus, Transport},
    };

    use super::*;

    struct RecordingTransport {
        commands: Arc<AtomicUsize>,
        status: Cell<DeviceStatus>,
        sticky_reset: bool,
        churn_generation: bool,
        generation_reads: Cell<u32>,
        offered_features: u64,
        driver_features: Arc<AtomicU64>,
        mac: [u8; 6],
    }

    impl RecordingTransport {
        fn stable(commands: Arc<AtomicUsize>) -> Self {
            Self {
                commands,
                status: Cell::new(DeviceStatus::empty()),
                sticky_reset: false,
                churn_generation: false,
                generation_reads: Cell::new(0),
                offered_features: VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC,
                driver_features: Arc::new(AtomicU64::new(0)),
                mac: [2, 3, 4, 5, 6, 7],
            }
        }

        fn record(&self) {
            self.commands.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl Transport for RecordingTransport {
        fn device_type(&self) -> DeviceType {
            DeviceType::Network
        }

        fn read_device_features(&mut self) -> u64 {
            self.record();
            self.offered_features
        }

        fn write_driver_features(&mut self, driver_features: u64) {
            self.record();
            self.driver_features
                .store(driver_features, Ordering::Release);
        }

        fn max_queue_size(&mut self, _queue: u16) -> u32 {
            QUEUE_SIZE as u32
        }

        fn notify(&mut self, _queue: u16) {
            self.record();
        }

        fn get_status(&self) -> DeviceStatus {
            self.status.get()
        }

        fn set_status(&mut self, status: DeviceStatus) {
            self.record();
            if self.sticky_reset && status.is_empty() {
                return;
            }
            self.status.set(status);
        }

        fn set_guest_page_size(&mut self, _guest_page_size: u32) {
            self.record();
        }

        fn requires_legacy_layout(&self) -> bool {
            false
        }

        fn queue_set(
            &mut self,
            _queue: u16,
            _size: u32,
            _descriptors: PhysAddr,
            _driver_area: PhysAddr,
            _device_area: PhysAddr,
        ) {
            self.record();
        }

        fn queue_unset(&mut self, _queue: u16) {
            self.record();
        }

        fn queue_used(&mut self, _queue: u16) -> bool {
            false
        }

        fn ack_interrupt(&mut self) -> InterruptStatus {
            panic!("owner transport must never acknowledge IRQ status")
        }

        fn read_config_generation(&self) -> u32 {
            let generation = self.generation_reads.get();
            if self.churn_generation {
                self.generation_reads.set(generation.wrapping_add(1));
            }
            generation
        }

        fn read_config_space<T>(&self, offset: usize) -> virtio_drivers::Result<T> {
            if offset != 0 || size_of::<T>() != self.mac.len() {
                return Err(VirtIoError::ConfigSpaceMissing);
            }
            let mut value = MaybeUninit::<T>::uninit();
            unsafe {
                // SAFETY: Transport requires T: FromBytes, so every initialized
                // byte pattern is valid. The size check above proves both
                // source and destination cover exactly the six MAC bytes.
                core::ptr::copy_nonoverlapping(
                    self.mac.as_ptr(),
                    value.as_mut_ptr().cast::<u8>(),
                    self.mac.len(),
                );
                Ok(value.assume_init())
            }
        }

        fn write_config_space<T>(
            &mut self,
            _offset: usize,
            _value: T,
        ) -> virtio_drivers::Result<()> {
            Err(VirtIoError::Unsupported)
        }
    }

    #[test]
    fn pending_transport_is_untouched_until_the_first_owner_poll() {
        let commands = Arc::new(AtomicUsize::new(0));
        let pending = VirtioNetPending {
            transport: RecordingTransport::stable(Arc::clone(&commands)),
            _transport_mapping: None,
        };

        assert_eq!(commands.load(Ordering::Relaxed), 0);
        let mut initialization = pending.begin(17);
        assert_eq!(commands.load(Ordering::Relaxed), 0);

        let progress = initialization.poll_owner_initialization(17).unwrap();
        let VirtioNetInitProgress::Pending(schedule) = progress else {
            panic!("the first owner pass must only issue reset")
        };
        assert_eq!(schedule.wake_at_ns, Some(RESET_RETRY_NS + 17));
        assert!(commands.load(Ordering::Relaxed) > 0);
        assert_eq!(initialization.stage, VirtioNetInitStage::WaitForReset);
    }

    #[test]
    fn pending_device_cannot_report_device_irq_mask_success() {
        let commands = Arc::new(AtomicUsize::new(0));
        let mut device = VirtIoNetDevice {
            owner: VirtioNetOwnerState::Pending(VirtioNetPending {
                transport: RecordingTransport::stable(Arc::clone(&commands)),
                _transport_mapping: None,
            }),
            interrupt_port: None,
            irq_enabled: false,
            mac: [0; 6],
            tx_created: false,
            rx_created: false,
        };

        assert!(rd_net::Interface::enable_irq(&mut device).is_err());
        assert!(rd_net::Interface::disable_irq(&mut device).is_err());
        assert!(!device.irq_enabled);
        assert_eq!(commands.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn event_idx_offer_is_rejected_without_a_complete_irq_source_mask() {
        const EVENT_IDX: u64 = 1 << 29;

        let commands = Arc::new(AtomicUsize::new(0));
        let mut transport = RecordingTransport::stable(commands);
        transport.offered_features |= EVENT_IDX;
        let driver_features = Arc::clone(&transport.driver_features);
        let mut initialization = VirtioNetPending {
            transport,
            _transport_mapping: None,
        }
        .begin(0);
        initialization.stage = VirtioNetInitStage::NegotiateFeatures;

        assert!(matches!(
            initialization.poll_owner_initialization(1),
            Ok(VirtioNetInitProgress::Pending(schedule)) if schedule.run_again
        ));
        assert_eq!(driver_features.load(Ordering::Acquire) & EVENT_IDX, 0);
    }

    #[test]
    fn changing_config_generation_has_a_finite_retry_budget() {
        let commands = Arc::new(AtomicUsize::new(0));
        let mut transport = RecordingTransport::stable(commands);
        transport.churn_generation = true;
        let mut initialization = VirtioNetPending {
            transport,
            _transport_mapping: None,
        }
        .begin(0);
        initialization.stage = VirtioNetInitStage::ReadConfiguration;

        for _ in 1..CONFIG_GENERATION_RETRY_LIMIT {
            assert!(matches!(
                initialization.poll_owner_initialization(1),
                Ok(VirtioNetInitProgress::Pending(schedule)) if schedule.run_again
            ));
        }
        assert!(matches!(
            initialization.poll_owner_initialization(1),
            Err(VirtioNetInitError::ConfigurationChurn)
        ));
        assert_eq!(
            initialization
                .transport
                .as_ref()
                .unwrap()
                .generation_reads
                .get(),
            u32::from(CONFIG_GENERATION_RETRY_LIMIT) * 2
        );
    }

    #[test]
    fn reset_wait_uses_an_absolute_terminal_deadline() {
        let commands = Arc::new(AtomicUsize::new(0));
        let mut transport = RecordingTransport::stable(commands);
        transport.sticky_reset = true;
        transport.status.set(DeviceStatus::DRIVER);
        let mut initialization = VirtioNetPending {
            transport,
            _transport_mapping: None,
        }
        .begin(100);

        assert!(matches!(
            initialization.poll_owner_initialization(100),
            Ok(VirtioNetInitProgress::Pending(_))
        ));
        assert!(matches!(
            initialization.poll_owner_initialization(100 + INITIALIZATION_TIMEOUT_NS),
            Err(VirtioNetInitError::Timeout)
        ));
    }
}
