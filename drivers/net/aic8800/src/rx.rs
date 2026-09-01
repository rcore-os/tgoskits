//! Bounded AIC receive-frame parsing.

use alloc::vec::Vec;

use crate::{
    common::{SDIO_TYPE_CFG_CMD_RSP, SDIO_TYPE_CFG_DATA_CFM, SDIO_TYPE_CFG_PRINT, SDIO_TYPE_DATA},
    lmac::is_indication_message,
};

pub(crate) const RX_CAPACITY: usize = 256;
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
pub(crate) fn parse_fifo(bytes: &[u8]) -> Result<Vec<ParsedFrame>, RxParseError> {
    let mut frames = Vec::new();
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
            if frames.len() >= RX_CAPACITY {
                break;
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
            frames.push(ParsedFrame::DataConfirmation);
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
            frames.push(ParsedFrame::FirmwarePrint { length: packet_len });
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
            frames.push(ParsedFrame::Data {
                frame: bytes[offset + HARDWARE_HEADER..offset + aggregate_len].to_vec(),
                decryption_status: ((status >> 2) & 0x7) as u8,
            });
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

        assert!(parse_fifo(&fifo).is_err());
    }

    #[test]
    fn command_response_with_payload_beyond_the_declared_packet_is_rejected() {
        let mut fifo = vec![0; 12];
        fifo[..2].copy_from_slice(&8u16.to_le_bytes());
        fifo[2] = SDIO_TYPE_CFG_CMD_RSP;
        fifo[4..6].copy_from_slice(&0x0403u16.to_le_bytes());
        fifo[10..12].copy_from_slice(&4u16.to_le_bytes());

        assert!(parse_fifo(&fifo).is_err());
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

        let frames = parse_fifo(&fifo).unwrap();
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
            parse_fifo(&fifo).unwrap().as_slice(),
            [ParsedFrame::FirmwarePrint { length: 8 }]
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
        let frames = parse_fifo(&fifo).unwrap();
        assert!(matches!(
            frames.as_slice(),
            [ParsedFrame::Data { frame, decryption_status: 0 }] if frame.len() == 24
        ));
    }
}
