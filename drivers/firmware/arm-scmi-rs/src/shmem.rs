use core::ptr::NonNull;

use mbarrier::{rmb, wmb};
use tock_registers::{interfaces::*, registers::*};

use crate::{Xfer, err::ScmiError};

// Field offsets are Linux `struct scmi_shared_mem` in
// `drivers/firmware/arm_scmi/shmem.c`: a leading reserved word, then
// channel_status, two more reserved words, flags, length, msg_header and the
// payload at 0x1C. The platform reads `length` to decide how much of the
// message to consume and `msg_header` to find the protocol, so a header that
// starts one word early reads back as an empty message that the platform
// silently drops, and this side then reads its own request as the response.
tock_registers::register_structs! {
    pub ShmemHeader {
        (0x00 => reserved: u32),
        (0x04 => channel_status: ReadWrite<u32, ChannelStatus::Register>),
        (0x08 => reserved1: [u32; 2]),
        (0x10 => flags: ReadWrite<u32, ShmemFlags::Register>),
        (0x14 => pub length: ReadWrite<u32>),
        (0x18 => pub msg_header: ReadWrite<u32>),
        (0x1C => @END),
    }
}

/// Offset of the channel status word, for the paths that poll it without
/// taking the header view.
const CHANNEL_STATUS: usize = 0x04;

/// Word index of the message header and of the first payload word, for a
/// relay that inspects a message another agent wrote.
pub const MSG_HEADER_WORD: usize = 0x18 / 4;
pub const PAYLOAD_WORD: usize = 0x1C / 4;

tock_registers::register_bitfields![
    u32,
    ChannelStatus [
        STATUS OFFSET(0) NUMBITS(2) [
            FREE = 0b01,
            ERROR = 0b10,
        ]
    ],
    ShmemFlags [
        /// Set when the agent asked the platform to signal completion with an
        /// interrupt. An agent without a completion interrupt leaves it clear,
        /// which is what Linux does for `no_completion_irq` transports.
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

    /// Raw 32-bit word of the channel, so a relay can inspect a message
    /// another agent has already written into it.
    pub(crate) fn word(&self, offset: usize) -> u32 {
        self.read_u32(offset)
    }

    pub(crate) fn header(&mut self) -> &mut ShmemHeader {
        unsafe { &mut *(self.address.as_ptr() as *mut ShmemHeader) }
    }
    /// Marks the channel free, as the platform does when a request has been
    /// consumed. An agent that answers a request itself must publish the same
    /// state, because the requester polls this bit before reading.
    pub fn mark_channel_free(&mut self) {
        self.header()
            .channel_status
            .write(ChannelStatus::STATUS::FREE);
    }

    /// Waits until the platform reports the channel free, i.e. until the
    /// request has completed and the response occupies the buffer.
    ///
    /// A platform that signals completion sets `CHANNEL_FREE` in the channel
    /// status word; reading the response before that yields the bytes this
    /// side wrote, which reads like a reply that echoes the request. The SMC
    /// transport also allows a platform to finish inside the call and leave
    /// the status word alone, so a timeout is not by itself an error.
    ///
    /// Returns `false` on timeout.
    pub fn wait_channel_free(&self, spins: u32) -> bool {
        for _ in 0..spins {
            if self.read_u32(CHANNEL_STATUS) & 1 != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    /// Publishes whether this agent takes a completion interrupt. A platform
    /// told to interrupt an agent which never subscribed can leave the request
    /// unfinished, and the agent then reads stale bytes as the response.
    pub fn set_completion_interrupt(&mut self, enabled: bool) {
        if enabled {
            self.header().flags.write(ShmemFlags::INTR_ENABLED::SET);
        } else {
            self.header().flags.write(ShmemFlags::INTR_ENABLED::CLEAR);
        }
    }

    pub fn tx_prepare(&mut self, xfer: &Xfer) -> Result<(), ScmiError> {
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
            self.write_payload(&xfer.tx)?;
        }
        Ok(())
    }

    pub(crate) fn write_message_header(
        &mut self,
        protocol_id: u32,
        message_id: u8,
        payload_len: u32,
    ) -> Result<(), ScmiError> {
        let len = size_of::<u32>()
            .checked_add(payload_len as usize)
            .ok_or(ScmiError::ProtocolError)?;
        if len > self.size {
            return Err(ScmiError::ProtocolError);
        }
        self.check_payload_range(0, payload_len as usize)?;

        self.header().channel_status.set(0);
        self.header().flags.set(0);
        self.header().length.set(len as u32);
        self.header()
            .msg_header
            .set(encode_message_header(protocol_id, message_id));
        Ok(())
    }

    pub(crate) fn write_payload_u32(&mut self, offset: usize, value: u32) -> Result<(), ScmiError> {
        if !offset.is_multiple_of(align_of::<u32>()) {
            return Err(ScmiError::ProtocolError);
        }
        self.check_payload_range(offset, size_of::<u32>())?;
        self.write_u32(Self::PAYLOAD_OFFSET + offset, value);
        Ok(())
    }

    pub(crate) fn read_payload_u32(&self, offset: usize) -> Result<u32, ScmiError> {
        if !offset.is_multiple_of(align_of::<u32>()) {
            return Err(ScmiError::ProtocolError);
        }
        self.check_payload_range(offset, size_of::<u32>())?;
        Ok(self.read_u32(Self::PAYLOAD_OFFSET + offset))
    }

    pub(crate) fn read_payload_i32(&self, offset: usize) -> Result<i32, ScmiError> {
        Ok(self.read_payload_u32(offset)? as i32)
    }

    pub fn payload_ptr(&mut self) -> *mut u8 {
        unsafe { self.address.as_ptr().add(Self::PAYLOAD_OFFSET) }
    }

    pub fn write_payload(&mut self, buff: &[u8]) -> Result<(), ScmiError> {
        self.check_payload_range(0, buff.len())?;
        unsafe {
            let dest = self.address.as_ptr().add(size_of::<ShmemHeader>());
            for (i, &b) in buff.iter().enumerate() {
                dest.add(i).write_volatile(b);
            }
        }
        wmb();
        Ok(())
    }

    pub fn read_payload(&mut self, buff: &mut [u8], skip: usize) -> Result<(), ScmiError> {
        self.check_payload_range(skip, buff.len())?;
        unsafe {
            let src = self.payload_ptr();
            for (i, b) in buff.iter_mut().enumerate() {
                *b = src.add(skip + i).read_volatile();
            }
        }
        rmb();
        Ok(())
    }

    fn check_payload_range(&self, offset: usize, len: usize) -> Result<(), ScmiError> {
        let payload_end = offset.checked_add(len).ok_or(ScmiError::ProtocolError)?;
        let window_end = Self::PAYLOAD_OFFSET
            .checked_add(payload_end)
            .ok_or(ScmiError::ProtocolError)?;
        if window_end > self.size {
            return Err(ScmiError::ProtocolError);
        }
        Ok(())
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

/// Encodes the SCMI message header word: `message_id` in bits 0-7,
/// `protocol_id` in bits 8-15 and the token, unused here, in bits 16-31.
const fn encode_message_header(protocol_id: u32, message_id: u8) -> u32 {
    // Linux `common.h` packs the header as message id in bits 7:0, message type
    // in bits 9:8, protocol id in bits 17:10 and the token in bits 27:18. All
    // commands use message type 0. A protocol id at bits 15:8 reaches the
    // platform as a different protocol, which then answers about the wrong
    // service or rejects the message, exactly like a buffer that stayed
    // untouched.
    ((protocol_id & 0xff) << 10) | (message_id as u32 & 0xff)
}

#[cfg(test)]
mod tests {
    use tock_registers::interfaces::{Readable, Writeable};

    use super::*;

    #[test]
    fn layout_matches_the_linux_shared_memory_structure() {
        // Linux `struct scmi_shared_mem` in `drivers/firmware/arm_scmi/shmem.c`
        // places the fields at reserved@0x00, channel_status@0x04,
        // reserved1[2]@0x08, flags@0x10, length@0x14, msg_header@0x18 and the
        // payload at 0x1C. A header written one word early leaves `length` at
        // zero where the platform reads it, and the platform then drops the
        // message instead of answering it.
        let mut buffer = [0u32; 8];
        let address = NonNull::new(buffer.as_mut_ptr().cast::<u8>()).unwrap();
        let mut shmem = unsafe { Shmem::new(address, 0, size_of_val(&buffer)) };

        assert_eq!(MSG_HEADER_WORD, 6);
        assert_eq!(PAYLOAD_WORD, 7);

        shmem.write_message_header(0x14, 0x05, 4).unwrap();
        shmem.write_payload_u32(0, 0xabcdef).unwrap();

        assert_eq!(buffer[1], 0, "channel_status marks the request in flight");
        assert_eq!(buffer[4], 0, "no completion interrupt is requested");
        assert_eq!(
            buffer[5], 8,
            "length covers the status word and the payload"
        );
        assert_eq!(buffer[6], 0x5005, "protocol id 0x14 and message id 0x05");
        assert_eq!(
            buffer[7], 0xabcdef,
            "payload starts after the message header"
        );
    }

    #[test]
    fn message_header_matches_the_specification_field_positions() {
        // Clock protocol, RATE_GET: protocol id 0x14 at bits 17:10, message id
        // 0x05 at bits 7:0.
        let header = encode_message_header(0x14, 0x05);
        assert_eq!(header, 0x5005);
        assert_eq!((header >> 10) & 0xff, 0x14);
        assert_eq!((header >> 8) & 0b11, 0);
        assert_eq!(header & 0xff, 0x05);
    }

    #[test]
    fn reset_marks_channel_status_free() {
        let mut buffer = [0u32; 8];
        let address = NonNull::new(buffer.as_mut_ptr().cast::<u8>()).unwrap();
        let mut shmem = unsafe { Shmem::new(address, 0, size_of_val(&buffer)) };

        shmem
            .header()
            .channel_status
            .write(ChannelStatus::STATUS::ERROR);

        shmem.reset();

        assert_eq!(
            shmem.header().channel_status.read(ChannelStatus::STATUS),
            ChannelStatus::STATUS::FREE.value
        );
    }
}
