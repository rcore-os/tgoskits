use alloc::vec::Vec;
use core::ptr::NonNull;

use crate::{descriptor::EndpointDescriptor, host::ControlSetup};

pub use crate::{descriptor::EndpointType, transfer::Direction};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointAddress(u8);

impl EndpointAddress {
    pub const CONTROL: Self = Self(0);

    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u8 {
        self.0
    }

    pub fn direction(self) -> Direction {
        Direction::from_address(self.0)
    }
}

impl From<u8> for EndpointAddress {
    fn from(value: u8) -> Self {
        Self::new(value)
    }
}

impl From<EndpointAddress> for u8 {
    fn from(value: EndpointAddress) -> Self {
        value.raw()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(u64);

impl RequestId {
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointInfo {
    pub address: EndpointAddress,
    pub transfer_type: EndpointType,
    pub direction: Direction,
    pub max_packet_size: u16,
    pub packets_per_microframe: usize,
    pub interval: u8,
}

impl EndpointInfo {
    pub const fn control() -> Self {
        Self {
            address: EndpointAddress::CONTROL,
            transfer_type: EndpointType::Control,
            direction: Direction::Out,
            max_packet_size: 64,
            packets_per_microframe: 1,
            interval: 0,
        }
    }
}

impl From<&EndpointDescriptor> for EndpointInfo {
    fn from(desc: &EndpointDescriptor) -> Self {
        Self {
            address: EndpointAddress::new(desc.address),
            transfer_type: desc.transfer_type,
            direction: desc.direction,
            max_packet_size: desc.max_packet_size,
            packets_per_microframe: desc.packets_per_microframe,
            interval: desc.interval,
        }
    }
}

#[derive(Clone, Copy)]
pub struct TransferBuffer {
    pub ptr: NonNull<u8>,
    pub len: usize,
}

unsafe impl Send for TransferBuffer {}
unsafe impl Sync for TransferBuffer {}

impl TransferBuffer {
    pub fn from_mut_slice(slice: &mut [u8]) -> Option<Self> {
        NonNull::new(slice.as_mut_ptr()).map(|ptr| Self {
            ptr,
            len: slice.len(),
        })
    }

    pub fn from_slice(slice: &[u8]) -> Option<Self> {
        NonNull::new(slice.as_ptr() as *mut u8).map(|ptr| Self {
            ptr,
            len: slice.len(),
        })
    }
}

#[derive(Clone)]
pub enum TransferKind {
    Control(ControlSetup),
    Bulk,
    Interrupt,
    Isochronous { packet_lengths: Vec<usize> },
}

impl TransferKind {
    pub fn get_control(&self) -> Option<&ControlSetup> {
        match self {
            TransferKind::Control(setup) => Some(setup),
            _ => None,
        }
    }

    pub fn iso_packet_lengths(&self) -> Option<&[usize]> {
        match self {
            TransferKind::Isochronous { packet_lengths } => Some(packet_lengths),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsoPacketRequest {
    pub length: usize,
}

#[derive(Clone)]
pub enum TransferRequest {
    Control {
        setup: ControlSetup,
        direction: Direction,
        buffer: Option<TransferBuffer>,
    },
    Bulk {
        direction: Direction,
        buffer: Option<TransferBuffer>,
    },
    Interrupt {
        direction: Direction,
        buffer: Option<TransferBuffer>,
    },
    Isochronous {
        direction: Direction,
        buffer: Option<TransferBuffer>,
        packets: Vec<IsoPacketRequest>,
    },
}

impl TransferRequest {
    pub fn control_in(setup: ControlSetup, buffer: &mut [u8]) -> Self {
        Self::Control {
            setup,
            direction: Direction::In,
            buffer: TransferBuffer::from_mut_slice(buffer),
        }
    }

    pub fn control_out(setup: ControlSetup, buffer: &[u8]) -> Self {
        Self::Control {
            setup,
            direction: Direction::Out,
            buffer: TransferBuffer::from_slice(buffer),
        }
    }

    pub fn bulk_in(buffer: &mut [u8]) -> Self {
        Self::Bulk {
            direction: Direction::In,
            buffer: TransferBuffer::from_mut_slice(buffer),
        }
    }

    pub fn bulk_out(buffer: &[u8]) -> Self {
        Self::Bulk {
            direction: Direction::Out,
            buffer: TransferBuffer::from_slice(buffer),
        }
    }

    pub fn interrupt_in(buffer: &mut [u8]) -> Self {
        Self::Interrupt {
            direction: Direction::In,
            buffer: TransferBuffer::from_mut_slice(buffer),
        }
    }

    pub fn interrupt_out(buffer: &[u8]) -> Self {
        Self::Interrupt {
            direction: Direction::Out,
            buffer: TransferBuffer::from_slice(buffer),
        }
    }

    pub fn iso_in(buffer: &mut [u8], packet_lengths: &[usize]) -> Self {
        Self::Isochronous {
            direction: Direction::In,
            buffer: TransferBuffer::from_mut_slice(buffer),
            packets: packet_lengths
                .iter()
                .copied()
                .map(|length| IsoPacketRequest { length })
                .collect(),
        }
    }

    pub fn iso_out(buffer: &[u8], packet_lengths: &[usize]) -> Self {
        Self::Isochronous {
            direction: Direction::Out,
            buffer: TransferBuffer::from_slice(buffer),
            packets: packet_lengths
                .iter()
                .copied()
                .map(|length| IsoPacketRequest { length })
                .collect(),
        }
    }

    pub fn direction(&self) -> Direction {
        match self {
            Self::Control { direction, .. }
            | Self::Bulk { direction, .. }
            | Self::Interrupt { direction, .. }
            | Self::Isochronous { direction, .. } => *direction,
        }
    }

    pub fn buffer(&self) -> Option<TransferBuffer> {
        match self {
            Self::Control { buffer, .. }
            | Self::Bulk { buffer, .. }
            | Self::Interrupt { buffer, .. }
            | Self::Isochronous { buffer, .. } => *buffer,
        }
    }

    pub fn iso_packets(&self) -> &[IsoPacketRequest] {
        match self {
            Self::Isochronous { packets, .. } => packets,
            _ => &[],
        }
    }
}

impl From<TransferRequest> for (TransferKind, Direction, Option<TransferBuffer>) {
    fn from(request: TransferRequest) -> Self {
        match request {
            TransferRequest::Control {
                setup,
                direction,
                buffer,
            } => (TransferKind::Control(setup), direction, buffer),
            TransferRequest::Bulk { direction, buffer } => (TransferKind::Bulk, direction, buffer),
            TransferRequest::Interrupt { direction, buffer } => {
                (TransferKind::Interrupt, direction, buffer)
            }
            TransferRequest::Isochronous {
                direction,
                buffer,
                packets,
            } => (
                TransferKind::Isochronous {
                    packet_lengths: packets.into_iter().map(|packet| packet.length).collect(),
                },
                direction,
                buffer,
            ),
        }
    }
}

impl From<(TransferKind, Direction, Option<TransferBuffer>)> for TransferRequest {
    fn from((kind, direction, buffer): (TransferKind, Direction, Option<TransferBuffer>)) -> Self {
        match kind {
            TransferKind::Control(setup) => Self::Control {
                setup,
                direction,
                buffer,
            },
            TransferKind::Bulk => Self::Bulk { direction, buffer },
            TransferKind::Interrupt => Self::Interrupt { direction, buffer },
            TransferKind::Isochronous { packet_lengths } => Self::Isochronous {
                direction,
                buffer,
                packets: packet_lengths
                    .into_iter()
                    .map(|length| IsoPacketRequest { length })
                    .collect(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferStatus {
    Completed,
    Stalled,
    Cancelled,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IsoPacketResult {
    pub requested_length: usize,
    pub actual_length: usize,
    pub status: TransferStatus,
}

#[derive(Clone, Debug)]
pub struct TransferCompletion {
    pub request_id: RequestId,
    pub status: TransferStatus,
    pub actual_length: usize,
    pub iso_packets: Vec<IsoPacketResult>,
}
