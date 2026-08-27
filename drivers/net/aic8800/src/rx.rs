//! Bounded AIC receive-frame parsing.

use alloc::{collections::VecDeque, vec::Vec};

use crate::common::{SDIO_TYPE_CFG, SDIO_TYPE_DATA};

pub(crate) const RX_CAPACITY: usize = 256;
const ALIGNMENT: usize = 4;

fn align_up(value: usize) -> usize {
    (value + ALIGNMENT - 1) & !(ALIGNMENT - 1)
}

pub(crate) enum ParsedFrame {
    Data(Vec<u8>),
    Confirmation { message_id: u16, payload: Vec<u8> },
    Indication { message_id: u16, payload: Vec<u8> },
}

/// Parses a FIFO aggregation without retaining aliases into the transfer buffer.
pub(crate) fn parse_fifo(bytes: &[u8]) -> Vec<ParsedFrame> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        let packet_len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if packet_len == 0 {
            break;
        }
        let packet_type = bytes[offset + 2] & 0x7f;
        if packet_type & SDIO_TYPE_CFG != 0 {
            let start = offset + 4;
            let end = start.saturating_add(packet_len);
            if end > bytes.len() || packet_len < 8 {
                break;
            }
            let message = &bytes[start..end];
            let message_id = u16::from_le_bytes([message[0], message[1]]);
            let declared = u16::from_le_bytes([message[6], message[7]]) as usize;
            let header = if message.len() >= 12 { 12 } else { 8 };
            let payload_end = (header + declared).min(message.len());
            let payload = message[header..payload_end].to_vec();
            if message_id & 1 == 1 {
                frames.push(ParsedFrame::Confirmation {
                    message_id,
                    payload,
                });
            } else {
                frames.push(ParsedFrame::Indication {
                    message_id,
                    payload,
                });
            }
            offset = offset.saturating_add(4 + align_up(packet_len));
        } else if packet_type == SDIO_TYPE_DATA {
            // RX data includes a vendor hardware header. Keep conversion in one
            // bounded parser and reject layouts too short to contain an MPDU.
            const HARDWARE_HEADER: usize = 60;
            let aggregate_len = packet_len.saturating_add(HARDWARE_HEADER);
            if offset + aggregate_len > bytes.len() || packet_len < 24 {
                break;
            }
            frames.push(ParsedFrame::Data(
                bytes[offset + HARDWARE_HEADER..offset + aggregate_len].to_vec(),
            ));
            offset = offset.saturating_add(align_up(aggregate_len));
        } else {
            break;
        }
    }
    frames
}

pub(crate) struct RxState {
    frames: VecDeque<Vec<u8>>,
}

impl RxState {
    pub(crate) const fn new() -> Self {
        Self {
            frames: VecDeque::new(),
        }
    }

    pub(crate) fn push(&mut self, frame: Vec<u8>) -> bool {
        if self.frames.len() >= RX_CAPACITY {
            return false;
        }
        self.frames.push_back(frame);
        true
    }

    pub(crate) fn pop(&mut self) -> Option<Vec<u8>> {
        self.frames.pop_front()
    }

    pub(crate) fn clear(&mut self) {
        self.frames.clear();
    }
}
