//! Emulated virtio-net device backed by split virtqueues and virtio-mmio.
//!
//! The device exposes one receive queue and one transmit queue. Queue metadata is
//! validated before guest memory is accessed, and malformed chains are rejected
//! without advancing the available ring.

use alloc::{boxed::Box, format, string::String, sync::Arc, vec, vec::Vec};
use core::{cell::RefCell, mem};

use ax_sync::SpinLock as Mutex;
use axdevice_base::{
    BusKind, ControllerInputId, Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult,
    DmaGrant, InterruptControllerId, InterruptSharing, InterruptTrigger, InterruptTriggerMode,
    IrqLine, Resource,
};
use axvm_types::GuestPhysAddr;

use crate::{
    AcpiContributionSpec, AcpiDeviceSpec, DeviceBundle, DeviceFirmwareSpec, DeviceLifecycle,
    DeviceManagerError, DeviceManagerResult, DeviceModel, DeviceRequirements, FdtContributionSpec,
    FdtNodeSpec, ResourceRequest, ResourceSlot, ServiceCardinality, ServiceKey,
};

mod descriptor;
mod mmio;

use descriptor::DescriptorDirection;

use crate::virtio::queue::QueueState;

const RX_QUEUE: usize = 0;
const TX_QUEUE: usize = 1;
const NUM_QUEUES: usize = 2;
const VIRTIO_NET_BASE_HEADER_LEN: usize = 10;
const VIRTIO_NET_MRG_RXBUF_HEADER_LEN: usize = 12;
const VIRTIO_NET_F_MRG_RXBUF: u64 = 1 << 15;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const MAX_ETHERNET_FRAME_LEN: usize = 1514;

/// Selects how the emulated device determines the virtio-net header layout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VirtioNetHeaderMode {
    /// Uses the 10-byte legacy header unless the driver accepts either
    /// `VIRTIO_NET_F_MRG_RXBUF` or `VIRTIO_F_VERSION_1`.
    #[default]
    Negotiated,
    /// Always uses the 12-byte header for a compatibility-pinned guest.
    ///
    /// Zephyr 4.3 accepts `VIRTIO_F_VERSION_1` and exchanges the modern 12-byte
    /// header without accepting `VIRTIO_NET_F_MRG_RXBUF`. Its competition
    /// profile pins that layout independently of feature-state tracking.
    FixedTwelveByte,
}

/// Immutable construction options for an emulated virtio-net device.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtioNetOptions {
    /// Isolated software-switch segment assigned to the device.
    pub segment_id: u16,
    /// Guest header interoperability policy.
    pub header_mode: VirtioNetHeaderMode,
}

#[derive(Clone, Copy)]
enum VirtioNetHeaderLayout {
    Legacy,
    TwelveByte,
}

impl VirtioNetHeaderLayout {
    const fn len(self) -> usize {
        match self {
            Self::Legacy => VIRTIO_NET_BASE_HEADER_LEN,
            Self::TwelveByte => VIRTIO_NET_MRG_RXBUF_HEADER_LEN,
        }
    }
}

/// MMIO window size for one emulated virtio-net device.
///
/// The full page is trapped so unused neighbouring virtio-mmio slots read as
/// absent devices instead of reaching a passthrough mapping.
pub const VIRTIO_NET_MMIO_SIZE: usize = 0x1000;

/// Typed service key for VM-local virtio network switch ports.
pub struct VirtioNetPortKey;

impl ServiceKey for VirtioNetPortKey {
    type Service = VirtioNetPort;

    const NAME: &'static str = "virtio-net-port";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Multiple;
}

/// One planned virtio network device backed by the VM-local interrupt controller.
pub struct VirtioNetModel {
    name: String,
    mac_suffix: u8,
    options: VirtioNetOptions,
    controller: InterruptControllerId,
    mmio_request: ResourceRequest<u64>,
    irq_request: ResourceRequest<ControllerInputId>,
}

impl VirtioNetModel {
    /// Creates a validated device model whose resources are resolved by the device graph.
    pub fn new(
        name: impl Into<String>,
        mac_suffix: u8,
        options: VirtioNetOptions,
        controller: InterruptControllerId,
        mmio_request: ResourceRequest<u64>,
        irq_request: ResourceRequest<ControllerInputId>,
    ) -> Self {
        Self {
            name: name.into(),
            mac_suffix,
            options,
            controller,
            mmio_request,
            irq_request,
        }
    }
}

impl DeviceModel for VirtioNetModel {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements> {
        DeviceRequirements::new()
            .with_mmio(
                ResourceSlot::new("registers")?,
                VIRTIO_NET_MMIO_SIZE as u64,
                VIRTIO_NET_MMIO_SIZE as u64,
                self.mmio_request,
            )?
            .with_wired_irq(
                ResourceSlot::new("irq")?,
                self.controller,
                InterruptTrigger::LevelTriggered,
                InterruptSharing::Exclusive,
                self.irq_request,
            )
    }

    fn firmware(&self) -> DeviceFirmwareSpec {
        let registers = ResourceSlot::new("registers").expect("static slot is valid");
        let interrupt = ResourceSlot::new("irq").expect("static slot is valid");
        DeviceFirmwareSpec::interfaces(
            Some(vec![FdtContributionSpec::Conventional(
                FdtNodeSpec::new("virtio_mmio")
                    .with_compatible("virtio,mmio")
                    .with_register(registers.clone())
                    .with_interrupt(interrupt.clone())
                    .with_empty_property("dma-coherent"),
            )]),
            Some(vec![AcpiContributionSpec::Conventional(
                AcpiDeviceSpec::new_indexed("VN", "LNRO0005")
                    .with_register(registers)
                    .with_interrupt(interrupt),
            )]),
        )
    }

    fn build(
        &self,
        context: &mut crate::DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let (base, length) = context.mmio("registers")?;
        let irq = context.irq("irq")?;
        let base = usize::try_from(base).map_err(|_| DeviceManagerError::InvalidConfig {
            operation: "build virtio network device",
            detail: format!("device '{}' MMIO base {base:#x} exceeds usize", self.name),
        })?;
        let length = usize::try_from(length).map_err(|_| DeviceManagerError::InvalidConfig {
            operation: "build virtio network device",
            detail: format!(
                "device '{}' MMIO length {length:#x} exceeds usize",
                self.name
            ),
        })?;
        build_virtio_net_mmio(
            self.name.clone(),
            base,
            length,
            irq.input().value(),
            [0x52, 0x54, 0, 0, 0, self.mac_suffix],
            self.options,
            irq,
        )
    }
}

#[derive(Default)]
struct VirtioNetState {
    device_features_sel: u32,
    driver_features: [u32; 2],
    driver_features_sel: u32,
    queue_sel: u32,
    queues: [QueueState; NUM_QUEUES],
    status: u32,
    interrupt_status: u32,
}

impl VirtioNetState {
    fn selected_queue(&self) -> Option<&QueueState> {
        self.queues.get(self.queue_sel as usize)
    }

    fn selected_queue_mut(&mut self) -> Option<&mut QueueState> {
        self.queues.get_mut(self.queue_sel as usize)
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn driver_accepted(&self, feature: u64) -> bool {
        let accepted =
            u64::from(self.driver_features[0]) | (u64::from(self.driver_features[1]) << u32::BITS);
        accepted & feature != 0
    }
}

/// An emulated virtio-net device exposed to one guest over virtio-mmio.
///
/// All guest-visible mutable state is protected by an IRQ-aware spin lock, so
/// the device can be shared by vCPU threads without an unsafe `Send` or `Sync`
/// implementation.
pub struct VirtioNet {
    base: GuestPhysAddr,
    size: usize,
    irq: usize,
    mac: [u8; 6],
    segment_id: u16,
    header_mode: VirtioNetHeaderMode,
    state: Mutex<VirtioNetState>,
}

impl VirtioNet {
    /// Creates a virtio-net device in the default network segment.
    pub fn new(base: GuestPhysAddr, mac: [u8; 6], irq: usize) -> Self {
        Self::new_with_options(base, mac, irq, VirtioNetOptions::default())
    }

    /// Creates a virtio-net device in `segment_id`.
    pub fn new_with_segment(
        base: GuestPhysAddr,
        mac: [u8; 6],
        irq: usize,
        segment_id: u16,
    ) -> Self {
        Self::new_with_options(
            base,
            mac,
            irq,
            VirtioNetOptions {
                segment_id,
                ..VirtioNetOptions::default()
            },
        )
    }

    /// Creates a virtio-net device with explicit switch and header options.
    pub fn new_with_options(
        base: GuestPhysAddr,
        mac: [u8; 6],
        irq: usize,
        options: VirtioNetOptions,
    ) -> Self {
        Self {
            base,
            size: VIRTIO_NET_MMIO_SIZE,
            irq,
            mac,
            segment_id: options.segment_id,
            header_mode: options.header_mode,
            state: Mutex::new(VirtioNetState::default()),
        }
    }

    /// Returns the guest IRQ line asserted for queue completion.
    pub fn irq(&self) -> usize {
        self.irq
    }

    /// Returns the base guest-physical address of the MMIO window.
    pub fn base(&self) -> GuestPhysAddr {
        self.base
    }

    /// Returns the Ethernet MAC address exposed through device configuration.
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Returns the isolated software-switch segment assigned to this device.
    pub fn segment_id(&self) -> u16 {
        self.segment_id
    }

    /// Returns whether an MMIO write notifies this device's transmit queue.
    pub fn is_tx_notify(&self, offset: usize, value: u32) -> bool {
        offset == mmio::VIRTIO_MMIO_QUEUE_NOTIFY && value as usize == TX_QUEUE
    }

    /// Drains valid transmit requests and returns their Ethernet frames.
    ///
    /// The virtio-net header is removed from each returned frame. A malformed
    /// descriptor chain is left available and reported as an error.
    ///
    /// # Errors
    ///
    /// Returns an error when queue metadata is malformed, guest memory cannot
    /// be accessed, or bounded packet allocation fails.
    pub fn process_tx(
        &self,
        read: &dyn Fn(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult,
        write: &dyn Fn(GuestPhysAddr, &[u8]) -> DeviceManagerResult,
    ) -> DeviceManagerResult<Vec<Vec<u8>>> {
        let mut state = self.state.lock_irqsave();
        let header_layout = self.header_layout(&state);
        let header_len = header_layout.len();
        let Some(queue) = state.queues[TX_QUEUE].active("process virtio-net TX")? else {
            return Ok(Vec::new());
        };
        let pending = queue.pending_count(read)?;
        let mut frames = Vec::new();
        frames.try_reserve_exact(pending as usize).map_err(|_| {
            DeviceManagerError::OutOfMemory {
                operation: "collect virtio-net TX frames",
            }
        })?;

        let mut available_index = queue.last_avail();
        for _ in 0..pending {
            let head = queue.available_head(read, available_index)?;
            let chain = descriptor::read_descriptor_chain(
                read,
                queue.descriptor_table(),
                queue.size(),
                head,
                DescriptorDirection::DeviceReadable,
                Some(header_len + MAX_ETHERNET_FRAME_LEN),
            )?;
            let mut packet = chain.read_packet(read)?;
            if packet.len() <= header_len {
                return Err(DeviceManagerError::InvalidInput {
                    operation: "process virtio-net TX packet",
                    detail: format!(
                        "packet length {} does not include an Ethernet frame",
                        packet.len()
                    ),
                });
            }

            queue.write_used(read, write, head, packet.len())?;
            available_index = available_index.wrapping_add(1);
            state.queues[TX_QUEUE].complete_available();
            state.interrupt_status |= 1;
            frames.push(packet.split_off(header_len));
        }
        Ok(frames)
    }

    /// Delivers one Ethernet frame into the receive queue.
    ///
    /// Returns `Ok(false)` only when the driver has not posted a receive buffer.
    /// An undersized or malformed posted chain is returned as an error and is
    /// not consumed.
    ///
    /// # Errors
    ///
    /// Returns an error when the frame exceeds the supported non-GSO size,
    /// queue metadata is malformed, guest memory cannot be accessed, or bounded
    /// packet allocation fails.
    pub fn deliver_rx(
        &self,
        read: &dyn Fn(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult,
        write: &dyn Fn(GuestPhysAddr, &[u8]) -> DeviceManagerResult,
        frame: &[u8],
    ) -> DeviceManagerResult<bool> {
        let mut state = self.state.lock_irqsave();
        let payload = receive_payload(frame, self.header_layout(&state))?;
        let Some(queue) = state.queues[RX_QUEUE].active("deliver virtio-net RX")? else {
            return Ok(false);
        };
        if queue.pending_count(read)? == 0 {
            return Ok(false);
        }

        let head = queue.available_head(read, queue.last_avail())?;
        let chain = descriptor::read_descriptor_chain(
            read,
            queue.descriptor_table(),
            queue.size(),
            head,
            DescriptorDirection::DeviceWritable,
            None,
        )?;
        if chain.capacity() < payload.len() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "deliver virtio-net RX packet",
                detail: format!(
                    "descriptor capacity {} is smaller than packet length {}",
                    chain.capacity(),
                    payload.len()
                ),
            });
        }

        chain.write_packet(write, &payload)?;
        queue.write_used(read, write, head, payload.len())?;
        state.queues[RX_QUEUE].complete_available();
        state.interrupt_status |= 1;
        Ok(true)
    }

    fn header_layout(&self, state: &VirtioNetState) -> VirtioNetHeaderLayout {
        match self.header_mode {
            VirtioNetHeaderMode::Negotiated
                if state.driver_accepted(VIRTIO_NET_F_MRG_RXBUF | VIRTIO_F_VERSION_1) =>
            {
                VirtioNetHeaderLayout::TwelveByte
            }
            VirtioNetHeaderMode::Negotiated => VirtioNetHeaderLayout::Legacy,
            VirtioNetHeaderMode::FixedTwelveByte => VirtioNetHeaderLayout::TwelveByte,
        }
    }

    fn interrupt_asserted(&self) -> bool {
        self.state.lock_irqsave().interrupt_status != 0
    }

    fn reset(&self) {
        self.state.lock_irqsave().reset();
    }
}

/// Builds one memory-mapped virtio network device and its VM-local switch port.
///
/// The caller supplies resources already resolved by the VM device graph. The
/// resulting bundle owns the guest-memory grant used only while handling a
/// source VM's queue notification; cross-VM delivery remains an explicit
/// operation on [`VirtioNetPort`].
pub fn build_virtio_net_mmio(
    name: String,
    base: usize,
    length: usize,
    irq_id: usize,
    mac: [u8; 6],
    options: VirtioNetOptions,
    irq: IrqLine,
) -> DeviceManagerResult<DeviceBundle> {
    if length != VIRTIO_NET_MMIO_SIZE {
        return Err(DeviceManagerError::InvalidConfig {
            operation: "build virtio network device",
            detail: format!(
                "device '{name}' MMIO length {length:#x} must be {VIRTIO_NET_MMIO_SIZE:#x}"
            ),
        });
    }

    let core = Arc::new(VirtioNet::new_with_options(
        base.into(),
        mac,
        irq_id,
        options,
    ));
    let port = Arc::new(VirtioNetPort::new(core, irq));
    let dma_grant = DmaGrant::new();
    let device = Arc::new(VirtioNetDevice::new(
        name,
        base,
        length,
        irq_id,
        Arc::clone(&port),
        dma_grant.clone(),
    )?);
    let mut bundle = DeviceBundle::new();
    bundle.add_guest_memory_device_with_grant(device.clone(), dma_grant);
    bundle.add_lifecycle(device);
    bundle.provide_service::<VirtioNetPortKey>(port)?;
    Ok(bundle)
}

/// VM-local network port capability exposed to the software switch.
///
/// Transmit descriptor processing stays inside the access-scoped device DMA
/// path. The switch drains only owned Ethernet frames from this capability.
/// Receive delivery is an explicit asynchronous backend operation supplied
/// with the destination VM's checked guest-memory callbacks.
pub struct VirtioNetPort {
    core: Arc<VirtioNet>,
    irq: IrqLine,
    transmitted_frames: Mutex<Vec<Vec<u8>>>,
}

impl VirtioNetPort {
    fn new(core: Arc<VirtioNet>, irq: IrqLine) -> Self {
        Self {
            core,
            irq,
            transmitted_frames: Mutex::new(Vec::new()),
        }
    }

    /// Returns the base guest-physical address of the MMIO window.
    pub fn base(&self) -> GuestPhysAddr {
        self.core.base()
    }

    /// Returns the Ethernet MAC address assigned to this port.
    pub fn mac(&self) -> [u8; 6] {
        self.core.mac()
    }

    /// Returns the isolated software-switch segment assigned to this port.
    pub fn segment_id(&self) -> u16 {
        self.core.segment_id()
    }

    /// Returns whether this access notifies the port's transmit queue.
    pub fn is_tx_notification(&self, address: GuestPhysAddr, value: u32) -> bool {
        address
            .as_usize()
            .checked_sub(self.base().as_usize())
            .is_some_and(|offset| self.core.is_tx_notify(offset, value))
    }

    /// Takes the complete batch produced by prior transmit notifications.
    pub fn take_transmitted_frames(&self) -> Vec<Vec<u8>> {
        mem::take(&mut self.transmitted_frames.lock_irqsave())
    }

    /// Delivers one frame to this port's receive virtqueue.
    pub fn deliver_rx(
        &self,
        read: &dyn Fn(GuestPhysAddr, &mut [u8]) -> DeviceManagerResult,
        write: &dyn Fn(GuestPhysAddr, &[u8]) -> DeviceManagerResult,
        frame: &[u8],
    ) -> DeviceManagerResult<bool> {
        let delivered = self.core.deliver_rx(read, write, frame)?;
        self.sync_interrupt_line()
            .map_err(DeviceManagerError::from)?;
        Ok(delivered)
    }

    fn process_tx_notification(
        &self,
        grant: &DmaGrant,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let context = RefCell::new(context);
        let read = |address, buffer: &mut [u8]| {
            context
                .borrow_mut()
                .read_guest_memory(grant, address, buffer)
                .map_err(DeviceManagerError::from)
        };
        let write = |address, buffer: &[u8]| {
            context
                .borrow_mut()
                .write_guest_memory(grant, address, buffer)
                .map_err(DeviceManagerError::from)
        };
        let tx_result = self
            .core
            .process_tx(&read, &write)
            .and_then(|frames| {
                let mut pending = self.transmitted_frames.lock_irqsave();
                pending
                    .try_reserve(frames.len())
                    .map_err(|_| DeviceManagerError::OutOfMemory {
                        operation: "queue transmitted virtio-net frames",
                    })?;
                pending.extend(frames);
                Ok(())
            })
            .map_err(DeviceError::from);
        let irq_result = self.sync_interrupt_line();
        tx_result.and(irq_result)
    }

    fn sync_interrupt_line(&self) -> DeviceResult {
        let result = if self.core.interrupt_asserted() {
            self.irq.assert()
        } else {
            self.irq.deassert()
        };
        result.map_err(|error| DeviceError::Backend {
            operation: "signal virtio network interrupt",
            detail: format!("{error}"),
        })
    }

    fn reset(&self) -> DeviceManagerResult {
        self.core.reset();
        self.transmitted_frames.lock_irqsave().clear();
        self.sync_interrupt_line().map_err(DeviceManagerError::from)
    }
}

/// Unified-device adapter that owns the DMA and interrupt capabilities for one
/// virtio network port.
struct VirtioNetDevice {
    name: String,
    base: u64,
    port: Arc<VirtioNetPort>,
    dma_grant: DmaGrant,
    resources: Box<[Resource]>,
}

impl VirtioNetDevice {
    fn new(
        name: String,
        base: usize,
        length: usize,
        irq_id: usize,
        port: Arc<VirtioNetPort>,
        dma_grant: DmaGrant,
    ) -> DeviceManagerResult<Self> {
        let irq_line = u32::try_from(irq_id).map_err(|_| DeviceManagerError::InvalidConfig {
            operation: "build virtio network device",
            detail: format!("device '{name}' IRQ {irq_id} does not fit the device resource format"),
        })?;
        Ok(Self {
            name,
            base: base as u64,
            port,
            dma_grant,
            resources: alloc::vec![
                Resource::MmioRange {
                    base: base as u64,
                    size: length as u64,
                },
                Resource::IrqLine {
                    line: irq_line,
                    trigger: InterruptTriggerMode::LevelTriggered,
                },
            ]
            .into_boxed_slice(),
        })
    }
}

impl Device for VirtioNetDevice {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let address = usize::try_from(access.address())
            .map(GuestPhysAddr::from_usize)
            .map_err(|_| DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        self.port
            .core
            .handle_read(address, access.width())
            .map(|value| value as u64)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let address = usize::try_from(access.address())
            .map(GuestPhysAddr::from_usize)
            .map_err(|_| DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        let value = usize::try_from(value).map_err(|_| DeviceError::InvalidInput {
            operation: "write virtio network register",
            detail: format!("value {value:#x} does not fit the host word"),
        })?;
        self.port
            .core
            .handle_write(address, access.width(), value)?;
        let offset = access
            .address()
            .checked_sub(self.base)
            .and_then(|offset| usize::try_from(offset).ok())
            .ok_or(DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        if self.port.core.is_tx_notify(offset, value as u32) {
            self.port
                .process_tx_notification(&self.dma_grant, context)?;
        } else {
            self.port.sync_interrupt_line()?;
        }
        Ok(())
    }
}

impl DeviceLifecycle for VirtioNetDevice {
    fn reset(&self) -> DeviceManagerResult {
        self.port.reset()
    }

    fn suspend(&self) -> DeviceManagerResult {
        Ok(())
    }

    fn resume(&self) -> DeviceManagerResult {
        Ok(())
    }
}

fn receive_payload(
    frame: &[u8],
    header_layout: VirtioNetHeaderLayout,
) -> DeviceManagerResult<Vec<u8>> {
    if frame.len() > MAX_ETHERNET_FRAME_LEN {
        return Err(DeviceManagerError::InvalidInput {
            operation: "deliver virtio-net RX packet",
            detail: format!(
                "Ethernet frame length {} exceeds non-GSO limit {MAX_ETHERNET_FRAME_LEN}",
                frame.len()
            ),
        });
    }

    let header_len = header_layout.len();
    let payload_len = header_len + frame.len();
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(payload_len)
        .map_err(|_| DeviceManagerError::OutOfMemory {
            operation: "allocate virtio-net RX packet",
        })?;
    payload.resize(payload_len, 0);
    if matches!(header_layout, VirtioNetHeaderLayout::TwelveByte) {
        payload[10..12].copy_from_slice(&1u16.to_le_bytes());
    }
    payload[header_len..].copy_from_slice(frame);
    Ok(payload)
}
