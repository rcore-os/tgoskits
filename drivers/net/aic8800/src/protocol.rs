//! AIC wire-format construction and parsing.

use alloc::{vec, vec::Vec};

use crate::common::{
    DRV_TASK_ID, SDIO_TYPE_CFG_CMD_RSP, SDIO_TYPE_DATA_TX, TASK_DBG, crc8_ponl_107,
};

pub(crate) const BLOCK_SIZE: usize = 512;
pub(crate) const DBG_MEM_READ_REQ: u16 = 0x0400;
pub(crate) const DBG_MEM_WRITE_REQ: u16 = 0x0402;
pub(crate) const DBG_MEM_BLOCK_WRITE_REQ: u16 = 0x040b;
pub(crate) const DBG_START_APP_REQ: u16 = 0x040d;
pub(crate) const DBG_MEM_MASK_WRITE_REQ: u16 = 0x0411;
const SDIO_HEADER_SIZE: usize = 4;
const DUMMY_WORD_SIZE: usize = 4;
const LMAC_HEADER_SIZE: usize = 8;
const HOST_DESCRIPTOR_SIZE: usize = 28;
const TX_ALIGNMENT: usize = 4;
const TAIL_SIZE: usize = 4;
const DEBUG_BLOCK_DATA_SIZE: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DebugConfirmationError {
    Malformed,
    Rejected(u32),
}

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
    debug_assert!(bytes.len() <= DEBUG_BLOCK_DATA_SIZE);
    let mut payload = vec![0; 8 + DEBUG_BLOCK_DATA_SIZE];
    payload[..4].copy_from_slice(&address.to_le_bytes());
    payload[4..8].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    payload[8..8 + bytes.len()].copy_from_slice(bytes);
    payload
}

pub(crate) fn start_app_payload(address: u32, boot_type: u32) -> [u8; 8] {
    let mut payload = [0; 8];
    payload[..4].copy_from_slice(&address.to_le_bytes());
    payload[4..].copy_from_slice(&boot_type.to_le_bytes());
    payload
}

pub(crate) fn debug_memory_read(
    payload: &[u8],
    expected_address: u32,
) -> Result<u32, DebugConfirmationError> {
    let words = exact_two_words(payload)?;
    if words[0] != expected_address {
        return Err(DebugConfirmationError::Malformed);
    }
    Ok(words[1])
}

pub(crate) fn require_debug_memory_write(
    payload: &[u8],
    expected_address: u32,
    expected_value: Option<u32>,
) -> Result<(), DebugConfirmationError> {
    let words = exact_two_words(payload)?;
    if words[0] != expected_address || expected_value.is_some_and(|value| words[1] != value) {
        return Err(DebugConfirmationError::Malformed);
    }
    Ok(())
}

pub(crate) fn require_debug_status(payload: &[u8]) -> Result<(), DebugConfirmationError> {
    let status = exact_word(payload)?;
    if status == 0 {
        Ok(())
    } else {
        Err(DebugConfirmationError::Rejected(status))
    }
}

fn exact_word(payload: &[u8]) -> Result<u32, DebugConfirmationError> {
    let bytes: [u8; 4] = payload
        .try_into()
        .map_err(|_| DebugConfirmationError::Malformed)?;
    Ok(u32::from_le_bytes(bytes))
}

fn exact_two_words(payload: &[u8]) -> Result<[u32; 2], DebugConfirmationError> {
    if payload.len() != 8 {
        return Err(DebugConfirmationError::Malformed);
    }
    Ok([
        u32::from_le_bytes(payload[..4].try_into().expect("length checked above")),
        u32::from_le_bytes(payload[4..].try_into().expect("length checked above")),
    ])
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
    // FULLMAC carries the Ethernet addresses and EtherType in `hostdesc`.
    // Match vendor `rwnx_tx.c`: save those fields, pull the 14-byte Ethernet
    // header, then submit only the remaining L3 payload after the descriptor.
    let payload = &ethernet[14..];
    let raw_len = SDIO_HEADER_SIZE + HOST_DESCRIPTOR_SIZE + payload.len();
    let aligned = align_up(raw_len, TX_ALIGNMENT);
    let final_len = if aligned.is_multiple_of(BLOCK_SIZE) {
        aligned
    } else {
        align_up(aligned + TAIL_SIZE, BLOCK_SIZE)
    };
    let mut frame = vec![0; final_len];
    // D80 preserves the unaligned FULLMAC packet length in its CRC-covered
    // header. V1/DC rewrites the header after word-aligning the aggregate.
    let advertised = if v3 {
        HOST_DESCRIPTOR_SIZE + payload.len()
    } else {
        aligned - SDIO_HEADER_SIZE
    };
    frame[0] = advertised as u8;
    frame[1] = ((advertised >> 8) & 0x0f) as u8;
    frame[2] = SDIO_TYPE_DATA_TX;
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
    fn debug_read_rejects_an_out_of_order_address() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&0x4050_0000u32.to_le_bytes());
        payload.extend_from_slice(&3u32.to_le_bytes());

        assert_eq!(
            debug_memory_read(&payload, 0x20),
            Err(DebugConfirmationError::Malformed)
        );
    }

    #[test]
    fn debug_status_rejects_firmware_failure() {
        assert_eq!(
            require_debug_status(&7u32.to_le_bytes()),
            Err(DebugConfirmationError::Rejected(7))
        );
    }

    #[test]
    fn block_write_uses_the_fixed_vendor_request_layout() {
        let payload = memory_block_write_payload(0x0018_0000, &[1, 2, 3, 4]);

        assert_eq!(payload.len(), 8 + 1024);
        assert_eq!(&payload[..4], &0x0018_0000_u32.to_le_bytes());
        assert_eq!(&payload[4..8], &4_u32.to_le_bytes());
        assert_eq!(&payload[8..12], &[1, 2, 3, 4]);
        assert!(payload[12..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn d80_ethernet_tx_matches_the_vendor_fullmac_descriptor() {
        let ethernet = [
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x08, 0x00,
            0xaa, 0xbb, 0xcc,
        ];

        let frame = ethernet_tx_frame(&ethernet, 2, 7, true).unwrap();

        assert_eq!(u16::from_le_bytes([frame[0], frame[1]]), 31);
        assert_eq!(frame[2], 0x01);
        assert_eq!(frame[3], crc8_ponl_107(&frame[..3]));
        let descriptor = &frame[SDIO_HEADER_SIZE..SDIO_HEADER_SIZE + HOST_DESCRIPTOR_SIZE];
        assert_eq!(&descriptor[..2], &3u16.to_le_bytes());
        assert_eq!(&descriptor[8..14], &ethernet[..6]);
        assert_eq!(&descriptor[14..20], &ethernet[6..12]);
        assert_eq!(&descriptor[20..22], &ethernet[12..14]);
        assert_eq!(descriptor[22], 0);
        assert_eq!(descriptor[23], 0);
        assert_eq!(descriptor[24], 2);
        assert_eq!(descriptor[25], 7);
        assert_eq!(&descriptor[26..28], &[0, 0]);
        assert_eq!(
            &frame[SDIO_HEADER_SIZE + HOST_DESCRIPTOR_SIZE..][..3],
            &[0xaa, 0xbb, 0xcc]
        );

        let dc_frame = ethernet_tx_frame(&ethernet, 2, 7, false).unwrap();
        assert_eq!(u16::from_le_bytes([dc_frame[0], dc_frame[1]]), 32);
        assert_eq!(dc_frame[2], 0x01);
        assert_eq!(dc_frame[3], 0);
    }
}
