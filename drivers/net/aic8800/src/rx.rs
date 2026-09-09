//! Bounded AIC receive-frame parsing.

use alloc::vec::Vec;

use crate::{
    common::{SDIO_TYPE_CFG_CMD_RSP, SDIO_TYPE_CFG_DATA_CFM, SDIO_TYPE_CFG_PRINT, SDIO_TYPE_DATA},
    lmac::is_indication_message,
};

pub(crate) const RX_CAPACITY: usize = 256;
pub(crate) const RX_BYTE_CAPACITY: usize = RX_CAPACITY * 2048;
pub(crate) const CONTROL_RX_CAPACITY: usize = 64;
pub(crate) const CONTROL_RX_BYTE_CAPACITY: usize = 64 * 1024;
const ALIGNMENT: usize = 4;
const E2A_HEADER_SIZE: usize = 12;

fn align_up(value: usize) -> usize {
    (value + ALIGNMENT - 1) & !(ALIGNMENT - 1)
}

pub(crate) enum ParsedFrame {
    Data {
        frame: Vec<u8>,
        decryption_status: u8,
    },
    DataConfirmation,
    FirmwarePrint {
        length: usize,
    },
    Confirmation {
        message_id: u16,
        payload: Vec<u8>,
    },
    Indication {
        message_id: u16,
        payload: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RxParseError {
    pub offset: usize,
    pub packet_type: u8,
    pub declared_length: usize,
    pub available_length: usize,
}

struct FrameBudget {
    items: usize,
    bytes: usize,
    item_limit: usize,
    byte_limit: usize,
    reserved_items: usize,
    reserved_bytes: usize,
}

impl FrameBudget {
    const fn new(item_limit: usize, byte_limit: usize) -> Self {
        Self {
            items: 0,
            bytes: 0,
            item_limit,
            byte_limit,
            reserved_items: 0,
            reserved_bytes: 0,
        }
    }

    fn admit(&mut self, bytes: usize) -> bool {
        let Some(total_bytes) = self.bytes.checked_add(bytes) else {
            return false;
        };
        if self.items >= self.item_limit || total_bytes > self.byte_limit {
            return false;
        }
        self.items += 1;
        self.bytes = total_bytes;
        true
    }

    fn admit_reserved(&mut self, bytes: usize) -> bool {
        let Some(total_reserved_bytes) = self.reserved_bytes.checked_add(bytes) else {
            return false;
        };
        if self.reserved_items > 0 || total_reserved_bytes > self.byte_limit {
            return false;
        }
        self.reserved_items = 1;
        self.reserved_bytes = total_reserved_bytes;
        true
    }
}

fn malformed_frame(
    offset: usize,
    packet_type: u8,
    declared_length: usize,
    available_length: usize,
) -> RxParseError {
    RxParseError {
        offset,
        packet_type,
        declared_length,
        available_length,
    }
}

/// Parses a FIFO aggregation without retaining aliases into the transfer buffer.
pub(crate) fn parse_fifo(
    bytes: &[u8],
    expected_confirmation: Option<u16>,
) -> Result<Vec<ParsedFrame>, RxParseError> {
    let mut frames = Vec::new();
    let mut data_budget = FrameBudget::new(RX_CAPACITY, RX_BYTE_CAPACITY);
    let mut control_budget = FrameBudget::new(CONTROL_RX_CAPACITY, CONTROL_RX_BYTE_CAPACITY);
    let mut offset = 0;
    while offset + 4 <= bytes.len() {
        let packet_len = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if packet_len == 0 {
            break;
        }
        let packet_type = bytes[offset + 2] & 0x7f;
        if packet_type == SDIO_TYPE_CFG_CMD_RSP {
            let start = offset + 4;
            let end = start.saturating_add(packet_len);
            if end > bytes.len() || packet_len < E2A_HEADER_SIZE {
                return Err(malformed_frame(
                    offset,
                    packet_type,
                    packet_len.max(E2A_HEADER_SIZE),
                    bytes.len().saturating_sub(start),
                ));
            }
            let message = &bytes[start..end];
            let message_id = u16::from_le_bytes([message[0], message[1]]);
            let declared = u16::from_le_bytes([message[6], message[7]]) as usize;
            let Some(payload_end) = E2A_HEADER_SIZE.checked_add(declared) else {
                return Err(malformed_frame(
                    offset,
                    packet_type,
                    usize::MAX,
                    message.len(),
                ));
            };
            if payload_end != message.len() {
                return Err(malformed_frame(
                    offset,
                    packet_type,
                    payload_end,
                    message.len(),
                ));
            }
            // Check the control-plane budget before copying an untrusted
            // firmware payload. Frames over the budget are deliberately
            // dropped, except for the confirmation currently awaited by the
            // mailbox, which has one reserved slot of its own.
            if control_budget.admit(declared)
                || (expected_confirmation == Some(message_id)
                    && control_budget.admit_reserved(declared))
            {
                let payload = message[E2A_HEADER_SIZE..payload_end].to_vec();
                if is_indication_message(message_id) {
                    frames.push(ParsedFrame::Indication {
                        message_id,
                        payload,
                    });
                } else {
                    frames.push(ParsedFrame::Confirmation {
                        message_id,
                        payload,
                    });
                }
            }
            offset = offset.saturating_add(4 + align_up(packet_len));
        } else if packet_type == SDIO_TYPE_CFG_DATA_CFM {
            let aggregate_len = 4usize.checked_add(align_up(packet_len)).ok_or_else(|| {
                malformed_frame(offset, packet_type, usize::MAX, bytes.len() - offset)
            })?;
            if offset.checked_add(aggregate_len).ok_or_else(|| {
                malformed_frame(offset, packet_type, usize::MAX, bytes.len() - offset)
            })? > bytes.len()
            {
                return Err(malformed_frame(
                    offset,
                    packet_type,
                    aggregate_len,
                    bytes.len() - offset,
                ));
            }
            if control_budget.admit(0) {
                frames.push(ParsedFrame::DataConfirmation);
            }
            offset += aggregate_len;
        } else if packet_type == SDIO_TYPE_CFG_PRINT {
            let aggregate_len = 4usize.checked_add(align_up(packet_len)).ok_or_else(|| {
                malformed_frame(offset, packet_type, usize::MAX, bytes.len() - offset)
            })?;
            if offset.checked_add(aggregate_len).ok_or_else(|| {
                malformed_frame(offset, packet_type, usize::MAX, bytes.len() - offset)
            })? > bytes.len()
            {
                return Err(malformed_frame(
                    offset,
                    packet_type,
                    aggregate_len,
                    bytes.len() - offset,
                ));
            }
            if control_budget.admit(0) {
                frames.push(ParsedFrame::FirmwarePrint { length: packet_len });
            }
            offset += aggregate_len;
        } else if packet_type == SDIO_TYPE_DATA {
            // RX data includes a vendor hardware header. Keep conversion in one
            // bounded parser and reject layouts too short to contain an MPDU.
            const HARDWARE_HEADER: usize = 60;
            let aggregate_len = packet_len.saturating_add(HARDWARE_HEADER);
            if offset + aggregate_len > bytes.len() || packet_len < 24 {
                return Err(malformed_frame(
                    offset,
                    packet_type,
                    aggregate_len.max(HARDWARE_HEADER + 24),
                    bytes.len() - offset,
                ));
            }
            let status = u32::from_le_bytes(
                bytes[offset + 36..offset + 40]
                    .try_into()
                    .expect("hardware header status is within the fixed header"),
            );
            if data_budget.admit(packet_len) {
                frames.push(ParsedFrame::Data {
                    frame: bytes[offset + HARDWARE_HEADER..offset + aggregate_len].to_vec(),
                    decryption_status: ((status >> 2) & 0x7) as u8,
                });
            }
            offset = offset.saturating_add(align_up(aggregate_len));
        } else {
            return Err(malformed_frame(
                offset,
                packet_type,
                packet_len,
                bytes.len() - offset,
            ));
        }
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    #[test]
    fn truncated_command_response_is_rejected() {
        let mut fifo = vec![0; 16];
        fifo[..2].copy_from_slice(&12u16.to_le_bytes());
        fifo[2] = SDIO_TYPE_CFG_CMD_RSP;
        fifo[10..12].copy_from_slice(&8u16.to_le_bytes());

        assert!(parse_fifo(&fifo, None).is_err());
    }

    #[test]
    fn command_response_with_payload_beyond_the_declared_packet_is_rejected() {
        let mut fifo = vec![0; 12];
        fifo[..2].copy_from_slice(&8u16.to_le_bytes());
        fifo[2] = SDIO_TYPE_CFG_CMD_RSP;
        fifo[4..6].copy_from_slice(&0x0403u16.to_le_bytes());
        fifo[10..12].copy_from_slice(&4u16.to_le_bytes());

        assert!(parse_fifo(&fifo, None).is_err());
    }

    #[test]
    fn dc_debug_memory_read_confirmation_uses_the_twelve_byte_e2a_header() {
        let fifo = [
            0x14,
            0x00,
            SDIO_TYPE_CFG_CMD_RSP,
            0x00,
            0x01,
            0x04,
            0x00,
            0x00,
            0x00,
            0x00,
            0x08,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x50,
            0x40,
            0x18,
            0x88,
            0xc7,
            0x07,
        ];

        let frames = parse_fifo(&fifo, None).unwrap();
        let [
            ParsedFrame::Confirmation {
                message_id,
                payload,
            },
        ] = frames.as_slice()
        else {
            panic!("expected one firmware confirmation")
        };
        assert_eq!(*message_id, 0x0401);
        assert_eq!(payload, &[0x00, 0x00, 0x50, 0x40, 0x18, 0x88, 0xc7, 0x07]);
    }

    #[test]
    fn firmware_print_is_consumed_without_treating_it_as_lmac() {
        let mut fifo = vec![0; 12];
        fifo[..2].copy_from_slice(&8u16.to_le_bytes());
        fifo[2] = SDIO_TYPE_CFG_PRINT;

        assert!(matches!(
            parse_fifo(&fifo, None).unwrap().as_slice(),
            [ParsedFrame::FirmwarePrint { length: 8 }]
        ));
    }

    #[test]
    fn control_response_payloads_have_a_byte_budget() {
        const PAYLOAD_LENGTH: usize = 1024;
        const RESPONSE_COUNT: usize = 128;
        let packet_length = 12 + PAYLOAD_LENGTH;
        let aggregate_length = 4 + packet_length.div_ceil(4) * 4;
        let mut fifo = vec![0; aggregate_length * RESPONSE_COUNT];

        for index in 0..RESPONSE_COUNT {
            let offset = index * aggregate_length;
            fifo[offset..offset + 2].copy_from_slice(&(packet_length as u16).to_le_bytes());
            fifo[offset + 2] = SDIO_TYPE_CFG_CMD_RSP;
            fifo[offset + 4..offset + 6].copy_from_slice(&0x0401u16.to_le_bytes());
            fifo[offset + 10..offset + 12].copy_from_slice(&(PAYLOAD_LENGTH as u16).to_le_bytes());
            fifo[offset + 16..offset + 16 + PAYLOAD_LENGTH].fill(index as u8);
        }

        let frames = parse_fifo(&fifo, None).unwrap();
        let retained_payload_bytes: usize = frames
            .iter()
            .map(|frame| match frame {
                ParsedFrame::Confirmation { payload, .. }
                | ParsedFrame::Indication { payload, .. } => payload.len(),
                _ => 0,
            })
            .sum();
        assert!(retained_payload_bytes <= CONTROL_RX_BYTE_CAPACITY);
    }

    #[test]
    fn expected_confirmation_has_a_reserved_slot_after_control_budget_is_full() {
        const PRINT_PACKET_LENGTH: usize = 8;
        const PRINT_AGGREGATE_LENGTH: usize = 4 + PRINT_PACKET_LENGTH;
        const RESPONSE_PACKET_LENGTH: usize = E2A_HEADER_SIZE;
        const RESPONSE_AGGREGATE_LENGTH: usize = 4 + RESPONSE_PACKET_LENGTH;
        const EXPECTED_MESSAGE_ID: u16 = 2;

        let response_offset = CONTROL_RX_CAPACITY * PRINT_AGGREGATE_LENGTH;
        let mut fifo = vec![0; response_offset + RESPONSE_AGGREGATE_LENGTH];
        for index in 0..CONTROL_RX_CAPACITY {
            let offset = index * PRINT_AGGREGATE_LENGTH;
            fifo[offset..offset + 2].copy_from_slice(&(PRINT_PACKET_LENGTH as u16).to_le_bytes());
            fifo[offset + 2] = SDIO_TYPE_CFG_PRINT;
        }
        fifo[response_offset..response_offset + 2]
            .copy_from_slice(&(RESPONSE_PACKET_LENGTH as u16).to_le_bytes());
        fifo[response_offset + 2] = SDIO_TYPE_CFG_CMD_RSP;
        fifo[response_offset + 4..response_offset + 6]
            .copy_from_slice(&EXPECTED_MESSAGE_ID.to_le_bytes());

        let frames = parse_fifo(&fifo, Some(EXPECTED_MESSAGE_ID)).unwrap();
        assert_eq!(frames.len(), CONTROL_RX_CAPACITY + 1);
        assert!(matches!(
            frames.last(),
            Some(ParsedFrame::Confirmation { message_id, payload })
                if *message_id == EXPECTED_MESSAGE_ID && payload.is_empty()
        ));
    }

    #[test]
    fn vendor_zero_data_type_is_parsed_as_an_ethernet_frame() {
        // AIC's 60-byte RX hardware header is present even for a short frame;
        // keep the fixture bounded to one aggregate.
        let mut fifo = vec![0; 84];
        fifo[..2].copy_from_slice(&24u16.to_le_bytes());
        fifo[2] = 0;
        fifo[60..74].copy_from_slice(&[0; 14]);
        let frames = parse_fifo(&fifo, None).unwrap();
        assert!(matches!(
            frames.as_slice(),
            [ParsedFrame::Data { frame, decryption_status: 0 }] if frame.len() == 24
        ));
    }
}
