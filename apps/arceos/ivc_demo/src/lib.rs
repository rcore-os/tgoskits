#![no_std]

//! Shared page layout used by the first-stage Axvisor IVC demo guests.
use core::sync::atomic::{AtomicU64, Ordering};

/// IVC channel key used by the first-stage demo.
pub const CHANNEL_KEY: usize = 0x4956_4301;
/// Publisher VM ID used by the first-stage demo configs.
pub const PUBLISHER_VM_ID: usize = 1;
/// Subscriber VM ID used by the first-stage demo configs.
pub const SUBSCRIBER_VM_ID: usize = 2;
/// Requested IVC channel size for phase 1.
pub const CHANNEL_SIZE: usize = 4096;
/// Maximum message payload stored in the shared demo page.
pub const MESSAGE_CAPACITY: usize = 128;

/// One shared IVC page used by the polling demo.
///
/// The first two fields intentionally match `axvm::runtime::ivc::IVCChannelHeader`.
/// Axvisor initializes them when the host-side channel is created; the demo only
/// uses the following fields for its polling message protocol.
#[repr(C, align(8))]
pub struct IvcDemoPage {
    publisher_id: u64,
    key: u64,
    sequence: AtomicU64,
    message_len: AtomicU64,
    message: [u8; MESSAGE_CAPACITY],
}

impl IvcDemoPage {
    /// Initializes the demo fields and preserves the Axvisor IVC header values.
    pub fn initialize(&mut self, publisher_id: usize, key: usize) {
        self.publisher_id = publisher_id as u64;
        self.key = key as u64;
        self.message_len.store(0, Ordering::Relaxed);
        self.sequence.store(0, Ordering::Release);
        self.message.fill(0);
    }

    /// Returns whether the page header matches the expected Axvisor IVC channel.
    pub fn header_matches(&self, publisher_id: usize, key: usize) -> bool {
        self.publisher_id == publisher_id as u64 && self.key == key as u64
    }

    /// Publishes one demo message and commits it by increasing the sequence.
    pub fn publish_message(&mut self, sequence: u64, message: &str) {
        let bytes = message.as_bytes();
        let len = bytes.len().min(MESSAGE_CAPACITY);
        self.message[..len].copy_from_slice(&bytes[..len]);
        if len < MESSAGE_CAPACITY {
            self.message[len..].fill(0);
        }
        self.message_len.store(len as u64, Ordering::Relaxed);
        self.sequence.store(sequence, Ordering::Release);
    }

    /// Copies the latest committed message into `buffer`.
    pub fn read_message(&self, buffer: &mut [u8]) -> IvcDemoSnapshot {
        let sequence = self.sequence.load(Ordering::Acquire);
        let len = (self.message_len.load(Ordering::Relaxed) as usize)
            .min(MESSAGE_CAPACITY)
            .min(buffer.len());
        buffer[..len].copy_from_slice(&self.message[..len]);
        IvcDemoSnapshot { sequence, len }
    }
}

/// A copied view of one demo page sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcDemoSnapshot {
    sequence: u64,
    len: usize,
}

impl IvcDemoSnapshot {
    /// Returns the committed sequence number.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Returns the copied message length.
    pub const fn len(self) -> usize {
        self.len
    }

    /// Returns whether this snapshot has an empty payload.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_then_read_returns_latest_sequence_and_payload() {
        let mut page = IvcDemoPage {
            publisher_id: 0,
            key: 0,
            sequence: AtomicU64::new(0),
            message_len: AtomicU64::new(0),
            message: [0; MESSAGE_CAPACITY],
        };
        page.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

        page.publish_message(7, "ivc hello");

        let mut buffer = [0; MESSAGE_CAPACITY];
        let snapshot = page.read_message(&mut buffer);
        assert_eq!(snapshot.sequence(), 7);
        assert_eq!(&buffer[..snapshot.len()], b"ivc hello");
    }

    #[test]
    fn publish_truncates_payload_to_page_capacity() {
        let mut page = IvcDemoPage {
            publisher_id: 0,
            key: 0,
            sequence: AtomicU64::new(0),
            message_len: AtomicU64::new(0),
            message: [0; MESSAGE_CAPACITY],
        };
        page.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

        page.publish_message(
            1,
            core::str::from_utf8(&[b'x'; MESSAGE_CAPACITY + 1]).unwrap(),
        );

        let mut buffer = [0; MESSAGE_CAPACITY + 8];
        let snapshot = page.read_message(&mut buffer);
        assert_eq!(snapshot.len(), MESSAGE_CAPACITY);
        assert!(buffer[..snapshot.len()].iter().all(|byte| *byte == b'x'));
    }
}
