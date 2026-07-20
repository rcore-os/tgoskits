//! Pure AIC8800 SDIO/LMAC wire-format helpers.

use alloc::{vec, vec::Vec};

use crate::common::{ChipVariant, DRV_TASK_ID, SDIO_TYPE_CFG_CMD_RSP, crc8_ponl_107};

pub(crate) const SDIO_BLOCK_SIZE: usize = 512;
pub(crate) const LMAC_HEADER_OFFSET: usize = 8;
pub(crate) const LMAC_HEADER_SIZE: usize = 8;
pub(crate) const LMAC_PAYLOAD_OFFSET: usize = LMAC_HEADER_OFFSET + LMAC_HEADER_SIZE;
pub(crate) const MAX_LMAC_PAYLOAD: usize = 1032;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WireError {
    #[error("LMAC payload exceeds the transport limit")]
    PayloadTooLarge,
    #[error("LMAC confirmation frame is truncated")]
    Truncated,
    #[error("LMAC confirmation id mismatch: expected {expected:#06x}, got {actual:#06x}")]
    ConfirmationMismatch { expected: u16, actual: u16 },
}

pub(crate) fn build_lmac_frame(
    chip: ChipVariant,
    message_id: u16,
    destination: u16,
    payload: &[u8],
) -> Result<Vec<u8>, WireError> {
    if payload.len() > MAX_LMAC_PAYLOAD {
        return Err(WireError::PayloadTooLarge);
    }

    let raw_len = LMAC_PAYLOAD_OFFSET + payload.len();
    let aligned = raw_len.next_multiple_of(4);
    let transfer_len = if aligned.is_multiple_of(SDIO_BLOCK_SIZE) {
        aligned
    } else {
        (aligned + 4).next_multiple_of(SDIO_BLOCK_SIZE)
    };
    let mut frame = vec![0; transfer_len];
    let transport_len = 4 + LMAC_HEADER_SIZE + payload.len();
    frame[0] = transport_len as u8;
    frame[1] = ((transport_len >> 8) & 0x0f) as u8;
    frame[2] = SDIO_TYPE_CFG_CMD_RSP;
    frame[3] = if chip.is_v3() {
        crc8_ponl_107(&frame[..3])
    } else {
        0
    };
    frame[LMAC_HEADER_OFFSET..LMAC_HEADER_OFFSET + 2].copy_from_slice(&message_id.to_le_bytes());
    frame[LMAC_HEADER_OFFSET + 2..LMAC_HEADER_OFFSET + 4]
        .copy_from_slice(&destination.to_le_bytes());
    frame[LMAC_HEADER_OFFSET + 4..LMAC_HEADER_OFFSET + 6]
        .copy_from_slice(&DRV_TASK_ID.to_le_bytes());
    frame[LMAC_HEADER_OFFSET + 6..LMAC_HEADER_OFFSET + 8]
        .copy_from_slice(&(payload.len() as u16).to_le_bytes());
    frame[LMAC_PAYLOAD_OFFSET..LMAC_PAYLOAD_OFFSET + payload.len()].copy_from_slice(payload);
    Ok(frame)
}

pub(crate) fn parse_confirmation(frame: &[u8], expected: u16) -> Result<&[u8], WireError> {
    if frame.len() < LMAC_PAYLOAD_OFFSET {
        return Err(WireError::Truncated);
    }
    let actual = u16::from_le_bytes([frame[4], frame[5]]);
    if actual != expected {
        return Err(WireError::ConfirmationMismatch { expected, actual });
    }
    let declared = u16::from_le_bytes([frame[10], frame[11]]) as usize;
    let end = LMAC_PAYLOAD_OFFSET
        .checked_add(declared)
        .filter(|end| *end <= frame.len())
        .ok_or(WireError::Truncated)?;
    Ok(&frame[LMAC_PAYLOAD_OFFSET..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_block_aligned_and_preserves_header() {
        let frame = build_lmac_frame(ChipVariant::Aic8800DC, 0x402, 1, &[1, 2, 3]).unwrap();
        assert_eq!(frame.len(), SDIO_BLOCK_SIZE);
        assert_eq!(&frame[8..10], &0x402u16.to_le_bytes());
        assert_eq!(&frame[16..19], &[1, 2, 3]);
    }

    #[test]
    fn confirmation_id_is_exact_not_best_effort() {
        let mut frame = vec![0; SDIO_BLOCK_SIZE];
        frame[4..6].copy_from_slice(&0x404u16.to_le_bytes());
        assert_eq!(
            parse_confirmation(&frame, 0x403),
            Err(WireError::ConfirmationMismatch {
                expected: 0x403,
                actual: 0x404,
            })
        );
    }
}
