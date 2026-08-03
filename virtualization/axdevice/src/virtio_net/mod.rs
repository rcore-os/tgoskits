//! Emulated virtio-net device backed by split virtqueues and virtio-mmio.
//!
//! The device exposes one receive queue and one transmit queue. Queue metadata is
//! validated before guest memory is accessed, and malformed chains are rejected
//! without advancing the available ring.

use alloc::{format, vec::Vec};

use ax_kspin::SpinNoIrq as Mutex;
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange};

use crate::{DeviceManagerError, DeviceManagerResult};

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
        let mut state = self.state.lock();
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
        let mut state = self.state.lock();
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

    fn guest_address_range(&self) -> GuestPhysAddrRange {
        GuestPhysAddrRange::from_start_size(self.base, self.size)
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
