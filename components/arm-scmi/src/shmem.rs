use core::ptr::NonNull;

use mbarrier::{rmb, wmb};
use tock_registers::{interfaces::*, registers::*};

use crate::Xfer;

tock_registers::register_structs! {
    pub ShmemHeader {
        (0x00 => reserved: u32),
        (0x04 => channel_status: ReadWrite<u32,  ChannelStatus::Register>),
        (0x08 => reserved1: [u32; 2]),
        (0x10 => flags: ReadWrite<u32, ShmemFlags::Register>),
        (0x14 => pub length: ReadWrite<u32>),
        (0x18 => pub msg_header: ReadWrite<u32>),
        (0x1C => @END),
    }
}

tock_registers::register_bitfields![
    u32,
    ChannelStatus [
        STATUS OFFSET(0) NUMBITS(2) [
            FREE = 0b01,
            ERROR = 0b10,
        ]
    ],
    ShmemFlags [
        INTR_ENABLED OFFSET(0) NUMBITS(1) [],
    ],
];

/// SCMI shared-memory transport window.
///
/// Maps a region of memory shared between the SCMI agent (OS) and the
/// SCMI platform (secure monitor / SCP). All reads/writes use volatile
/// access with the appropriate memory barriers.
///
/// # Safety
///
/// The caller must ensure that `address` points to a valid, mapped
/// shared-memory region whose lifetime exceeds that of this `Shmem`.
pub struct Shmem {
    /// Virtual address of the shared-memory window.
    pub address: NonNull<u8>,
    /// Bus (physical) address of the window, for DMA or device-tree use.
    pub bus_address: usize,
    /// Size of the window in bytes.
    pub size: usize,
}

// The mapped SCMI shared-memory window is accessed through volatile operations
// and external synchronization supplied by the owning SCMI instance.
unsafe impl Send for Shmem {}

impl Shmem {
    const PAYLOAD_OFFSET: usize = size_of::<ShmemHeader>();

    /// Create a new shared-memory handle.
    ///
    /// # Safety
    ///
    /// `address` must point to a valid mapped region of at least `size` bytes.
    pub unsafe fn new(address: NonNull<u8>, bus_address: usize, size: usize) -> Self {
        Self {
            address,
            bus_address,
            size,
        }
    }

    pub fn reset(&mut self) {
        trace!("Reset SHMEM at {:p}", self.address);
        self.header()
            .channel_status
            .write(ChannelStatus::STATUS::FREE);
        self.header().flags.set(0);
        self.header().length.set(0);
        self.header().msg_header.set(0);
    }

    pub(crate) fn header(&mut self) -> &mut ShmemHeader {
        unsafe { &mut *(self.address.as_ptr() as *mut ShmemHeader) }
    }
    pub fn tx_prepare(&mut self, xfer: &Xfer) {
        self.header().channel_status.set(0);
        self.header().flags.set(0);
        let len = size_of::<u32>() as u32 + xfer.tx.len() as u32;
        self.header().length.set(len);
        self.header().msg_header.set(xfer.hdr.pack());

        trace!(
            "Preparing TX: hdr={:?}, tx_len={}, all_len={len}",
            xfer.hdr,
            xfer.tx.len()
        );
        // Copy TX payload
        if !xfer.tx.is_empty() {
            self.write_payload(&xfer.tx);
        }
    }

    pub(crate) fn write_message_header(
        &mut self,
        protocol_id: u32,
        message_id: u8,
        payload_len: u32,
    ) {
        self.header().channel_status.set(0);
        self.header().flags.set(0);
        self.header().length.set(4 + payload_len);
        self.header()
            .msg_header
            .set(encode_message_header(protocol_id, message_id));
    }

    pub(crate) fn write_payload_u32(&mut self, offset: usize, value: u32) {
        self.write_u32(Self::PAYLOAD_OFFSET + offset, value);
    }

    pub(crate) fn read_payload_u32(&self, offset: usize) -> u32 {
        self.read_u32(Self::PAYLOAD_OFFSET + offset)
    }

    pub(crate) fn read_payload_i32(&self, offset: usize) -> i32 {
        self.read_payload_u32(offset) as i32
    }

    pub fn payload_ptr(&mut self) -> *mut u8 {
        unsafe { self.address.as_ptr().add(Self::PAYLOAD_OFFSET) }
    }

    pub fn write_payload(&mut self, buff: &[u8]) {
        unsafe {
            let dest = self.address.as_ptr().add(size_of::<ShmemHeader>());
            for (i, &b) in buff.iter().enumerate() {
                dest.add(i).write_volatile(b);
            }
        }
        wmb();
    }

    pub fn read_payload(&mut self, buff: &mut [u8], skip: usize) {
        unsafe {
            let src = self.payload_ptr();
            for (i, b) in buff.iter_mut().enumerate() {
                *b = src.add(skip + i).read_volatile();
            }
        }
        rmb();
    }

    fn write_u32(&mut self, offset: usize, value: u32) {
        unsafe {
            (self.address.as_ptr().add(offset) as *mut u32).write_volatile(value.to_le());
        }
    }

    fn read_u32(&self, offset: usize) -> u32 {
        unsafe { u32::from_le((self.address.as_ptr().add(offset) as *const u32).read_volatile()) }
    }
}

impl Shmem {
    pub const COMPATIBLE: &str = "arm,scmi-shmem";
}

const fn encode_message_header(protocol_id: u32, message_id: u8) -> u32 {
    ((protocol_id & 0xff) << 10) | (message_id as u32 & 0xff)
}
