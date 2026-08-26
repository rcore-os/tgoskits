//! AIC wire-format construction and parsing.

use alloc::{vec, vec::Vec};

use crate::common::{DRV_TASK_ID, SDIO_TYPE_CFG_CMD_RSP, SDIO_TYPE_DATA, TASK_DBG, crc8_ponl_107};

pub(crate) const BLOCK_SIZE: usize = 512;
pub(crate) const DBG_MEM_READ_REQ: u16 = 0x0400;
pub(crate) const DBG_MEM_WRITE_REQ: u16 = 0x0402;
pub(crate) const DBG_MEM_BLOCK_WRITE_REQ: u16 = 0x040b;
pub(crate) const DBG_START_APP_REQ: u16 = 0x040d;
pub(crate) const DBG_MEM_MASK_WRITE_REQ: u16 = 0x0411;
pub(crate) const MM_SET_STACK_START_REQ: u16 = 0x007b;
pub(crate) const TASK_MM: u16 = 0;

const SDIO_HEADER_SIZE: usize = 4;
const DUMMY_WORD_SIZE: usize = 4;
const LMAC_HEADER_SIZE: usize = 8;
const RESPONSE_PAYLOAD_OFFSET: usize = 16;
const HOST_DESCRIPTOR_SIZE: usize = 28;
const TX_ALIGNMENT: usize = 4;
const TAIL_SIZE: usize = 4;

fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

/// Builds one firmware or LMAC command accepted by the AIC FIFO.
pub(crate) fn command_frame(
    message_id: u16,
    destination: u16,
    payload: &[u8],
    v3: bool,
) -> Vec<u8> {
    let lmac_len = LMAC_HEADER_SIZE + payload.len();
    let wire_len = SDIO_HEADER_SIZE + lmac_len;
    let raw_len = SDIO_HEADER_SIZE + DUMMY_WORD_SIZE + lmac_len;
    let aligned = align_up(raw_len, TX_ALIGNMENT);
    let final_len = if aligned.is_multiple_of(BLOCK_SIZE) {
        aligned
    } else {
        align_up(aligned + TAIL_SIZE, BLOCK_SIZE)
    };
    let mut frame = vec![0; final_len];
    frame[0] = wire_len as u8;
    frame[1] = ((wire_len >> 8) & 0x0f) as u8;
    frame[2] = SDIO_TYPE_CFG_CMD_RSP;
    frame[3] = if v3 { crc8_ponl_107(&frame[..3]) } else { 0 };
    let header = SDIO_HEADER_SIZE + DUMMY_WORD_SIZE;
    frame[header..header + 2].copy_from_slice(&message_id.to_le_bytes());
    frame[header + 2..header + 4].copy_from_slice(&destination.to_le_bytes());
    frame[header + 4..header + 6].copy_from_slice(&DRV_TASK_ID.to_le_bytes());
    frame[header + 6..header + 8].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    frame[header + LMAC_HEADER_SIZE..header + LMAC_HEADER_SIZE + payload.len()]
        .copy_from_slice(payload);
    frame
}

pub(crate) fn debug_command_frame(message_id: u16, payload: &[u8], v3: bool) -> Vec<u8> {
    debug_assert!(message_id >= (TASK_DBG << 10));
    command_frame(message_id, TASK_DBG, payload, v3)
}

/// Extracts the confirmation payload from a boot/FDRV FIFO frame.
pub(crate) fn confirmation_payload(frame: &[u8], expected_message_id: u16) -> Result<Vec<u8>, ()> {
    if frame.len() < RESPONSE_PAYLOAD_OFFSET {
        return Err(());
    }
    let actual = u16::from_le_bytes([frame[4], frame[5]]);
    if actual != expected_message_id {
        return Err(());
    }
    let declared = u16::from_le_bytes([frame[10], frame[11]]) as usize;
    let available = frame.len().saturating_sub(RESPONSE_PAYLOAD_OFFSET);
    let length = declared.min(available);
    Ok(frame[RESPONSE_PAYLOAD_OFFSET..RESPONSE_PAYLOAD_OFFSET + length].to_vec())
}

pub(crate) fn memory_read_payload(address: u32) -> [u8; 4] {
    address.to_le_bytes()
}

pub(crate) fn memory_write_payload(address: u32, value: u32) -> [u8; 8] {
    let mut payload = [0; 8];
    payload[..4].copy_from_slice(&address.to_le_bytes());
    payload[4..].copy_from_slice(&value.to_le_bytes());
    payload
}

pub(crate) fn memory_mask_write_payload(address: u32, mask: u32, value: u32) -> [u8; 12] {
    let mut payload = [0; 12];
    payload[..4].copy_from_slice(&address.to_le_bytes());
    payload[4..8].copy_from_slice(&mask.to_le_bytes());
    payload[8..].copy_from_slice(&value.to_le_bytes());
    payload
}

pub(crate) fn memory_block_write_payload(address: u32, bytes: &[u8]) -> Vec<u8> {
    debug_assert!(bytes.len() <= 1024);
    let mut payload = Vec::with_capacity(8 + bytes.len());
    payload.extend_from_slice(&address.to_le_bytes());
    payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    payload.extend_from_slice(bytes);
    payload
}

pub(crate) fn start_app_payload(address: u32, boot_type: u32) -> [u8; 8] {
    let mut payload = [0; 8];
    payload[..4].copy_from_slice(&address.to_le_bytes());
    payload[4..].copy_from_slice(&boot_type.to_le_bytes());
    payload
}

/// Encapsulates one Ethernet packet for the firmware data ingress path.
pub(crate) fn ethernet_tx_frame(
    ethernet: &[u8],
    interface_index: u8,
    station_index: u8,
    v3: bool,
) -> Result<Vec<u8>, ()> {
    if ethernet.len() < 14 {
        return Err(());
    }
    let payload = &ethernet[14..];
    let raw_len = SDIO_HEADER_SIZE + HOST_DESCRIPTOR_SIZE + payload.len();
    let aligned = align_up(raw_len, TX_ALIGNMENT);
    let final_len = if aligned.is_multiple_of(BLOCK_SIZE) {
        aligned
    } else {
        align_up(aligned + TAIL_SIZE, BLOCK_SIZE)
    };
    let mut frame = vec![0; final_len];
    let advertised = aligned - SDIO_HEADER_SIZE;
    frame[0] = advertised as u8;
    frame[1] = ((advertised >> 8) & 0x0f) as u8;
    frame[2] = SDIO_TYPE_DATA;
    frame[3] = if v3 { crc8_ponl_107(&frame[..3]) } else { 0 };

    let descriptor = &mut frame[SDIO_HEADER_SIZE..SDIO_HEADER_SIZE + HOST_DESCRIPTOR_SIZE];
    descriptor[..2].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    descriptor[4..8].copy_from_slice(&0x8000_0001u32.to_le_bytes());
    descriptor[8..14].copy_from_slice(&ethernet[..6]);
    descriptor[14..20].copy_from_slice(&ethernet[6..12]);
    descriptor[20..22].copy_from_slice(&ethernet[12..14]);
    descriptor[24] = interface_index;
    descriptor[25] = station_index;
    frame[SDIO_HEADER_SIZE + HOST_DESCRIPTOR_SIZE
        ..SDIO_HEADER_SIZE + HOST_DESCRIPTOR_SIZE + payload.len()]
        .copy_from_slice(payload);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_frame_is_block_aligned_and_typed() {
        let frame = command_frame(0x1234, 7, &[1, 2, 3], true);
        assert_eq!(frame.len(), BLOCK_SIZE);
        assert_eq!(frame[2], SDIO_TYPE_CFG_CMD_RSP);
        assert_eq!(u16::from_le_bytes([frame[8], frame[9]]), 0x1234);
        assert_eq!(&frame[16..19], &[1, 2, 3]);
        assert_eq!(frame[3], crc8_ponl_107(&frame[..3]));
    }

    #[test]
    fn confirmation_rejects_unexpected_message() {
        let mut frame = vec![0; 32];
        frame[4..6].copy_from_slice(&0x401u16.to_le_bytes());
        frame[10..12].copy_from_slice(&4u16.to_le_bytes());
        frame[16..20].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(confirmation_payload(&frame, 0x401), Ok(vec![1, 2, 3, 4]));
        assert!(confirmation_payload(&frame, 0x403).is_err());
    }
}
