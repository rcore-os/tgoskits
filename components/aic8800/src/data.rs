//! Pure AIC8800 packet wire-format conversion.
//!
//! This module owns no queue, task, interrupt, or DMA resource. The
//! maintenance owner copies Ethernet packets into and out of these transport
//! frames while it retains CPU ownership of the runtime buffers.

use alloc::{vec, vec::Vec};

use crate::common::{ChipVariant, SDIO_TYPE_CFG, SDIO_TYPE_DATA, crc8_ponl_107};

const SDIO_HEADER_BYTES: usize = 4;
const HOST_DESCRIPTOR_BYTES: usize = 28;
const ETHERNET_HEADER_BYTES: usize = 14;
const TX_ALIGNMENT: usize = 4;
const TX_TAIL_BYTES: usize = 4;
const RX_HARDWARE_HEADER_BYTES: usize = 60;
const RX_ALIGNMENT: usize = 4;
const MAX_PACKET_BYTES: usize = 1600;
const TX_CONFIRMATION_HOST_ID: u32 = 0x8000_0001;

/// A malformed or unsupported data-plane frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DataWireError {
    /// An Ethernet frame did not contain its complete fixed header.
    #[error("Ethernet frame is shorter than its fixed header")]
    EthernetFrameTooShort,
    /// A firmware RX aggregate ended inside a declared frame.
    #[error("AIC8800 RX aggregate is truncated")]
    TruncatedAggregate,
    /// A firmware frame declared a packet larger than the supported MTU.
    #[error("AIC8800 RX packet exceeds the supported packet size")]
    PacketTooLarge,
}

/// Builds one block-aligned firmware TX frame from an Ethernet packet.
pub(crate) fn build_tx_frame(
    chip: ChipVariant,
    ethernet: &[u8],
    vif_index: u8,
    station_index: u8,
) -> Result<Vec<u8>, DataWireError> {
    if ethernet.len() < ETHERNET_HEADER_BYTES {
        return Err(DataWireError::EthernetFrameTooShort);
    }

    let payload = &ethernet[ETHERNET_HEADER_BYTES..];
    let transport_payload = HOST_DESCRIPTOR_BYTES + payload.len();
    let raw_len = SDIO_HEADER_BYTES + transport_payload;
    let aligned_len = raw_len.next_multiple_of(TX_ALIGNMENT);
    let transfer_len = if aligned_len.is_multiple_of(crate::wire::SDIO_BLOCK_SIZE) {
        aligned_len
    } else {
        (aligned_len + TX_TAIL_BYTES).next_multiple_of(crate::wire::SDIO_BLOCK_SIZE)
    };
    let mut frame = vec![0; transfer_len];

    let declared = aligned_len - SDIO_HEADER_BYTES;
    frame[0] = declared as u8;
    frame[1] = ((declared >> 8) & 0x0f) as u8;
    frame[2] = SDIO_TYPE_DATA;
    frame[3] = if chip.is_v3() {
        crc8_ponl_107(&frame[..3])
    } else {
        0
    };

    let descriptor = &mut frame[SDIO_HEADER_BYTES..SDIO_HEADER_BYTES + HOST_DESCRIPTOR_BYTES];
    descriptor[..2].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    descriptor[4..8].copy_from_slice(&TX_CONFIRMATION_HOST_ID.to_le_bytes());
    descriptor[8..14].copy_from_slice(&ethernet[..6]);
    descriptor[14..20].copy_from_slice(&ethernet[6..12]);
    descriptor[20..22].copy_from_slice(&ethernet[12..14]);
    descriptor[23] = 0;
    descriptor[24] = vif_index;
    descriptor[25] = station_index;

    let payload_start = SDIO_HEADER_BYTES + HOST_DESCRIPTOR_BYTES;
    frame[payload_start..payload_start + payload.len()].copy_from_slice(payload);
    Ok(frame)
}

/// Extracts all Ethernet packets carried by one acknowledged firmware RX
/// aggregate. Configuration and management frames remain controller-owned and
/// are ignored by this data-only conversion.
pub(crate) fn decode_rx_aggregate(bytes: &[u8]) -> Result<Vec<Vec<u8>>, DataWireError> {
    let mut packets = Vec::new();
    let mut offset = 0;
    while offset + SDIO_HEADER_BYTES <= bytes.len() {
        let packet_len = usize::from(u16::from_le_bytes([bytes[offset], bytes[offset + 1]]));
        if packet_len == 0 {
            break;
        }
        if packet_len > MAX_PACKET_BYTES {
            return Err(DataWireError::PacketTooLarge);
        }
        let packet_type = bytes[offset + 2] & 0x7f;
        if packet_type & SDIO_TYPE_CFG == SDIO_TYPE_CFG {
            let frame_len = packet_len
                .next_multiple_of(RX_ALIGNMENT)
                .checked_add(SDIO_HEADER_BYTES)
                .ok_or(DataWireError::TruncatedAggregate)?;
            if offset + frame_len > bytes.len() {
                return Err(DataWireError::TruncatedAggregate);
            }
            offset += frame_len;
            continue;
        }

        let aggregate_len = packet_len
            .checked_add(RX_HARDWARE_HEADER_BYTES)
            .ok_or(DataWireError::TruncatedAggregate)?;
        if offset + aggregate_len > bytes.len() {
            return Err(DataWireError::TruncatedAggregate);
        }
        let frame = &bytes[offset..offset + aggregate_len];
        if let Some(packet) = decode_data_frame(frame, packet_len)? {
            packets.push(packet);
        }
        offset += aggregate_len.next_multiple_of(RX_ALIGNMENT);
    }
    Ok(packets)
}

fn decode_data_frame(frame: &[u8], packet_len: usize) -> Result<Option<Vec<u8>>, DataWireError> {
    let mpdu_end = RX_HARDWARE_HEADER_BYTES
        .checked_add(packet_len)
        .ok_or(DataWireError::TruncatedAggregate)?;
    let mpdu = frame
        .get(RX_HARDWARE_HEADER_BYTES..mpdu_end)
        .ok_or(DataWireError::TruncatedAggregate)?;
    if mpdu.len() < 24 || mpdu[0] & 0x0c != 0x08 {
        return Ok(None);
    }

    let frame_control_1 = mpdu[1];
    let header_len =
        if mpdu[0] & 0x80 != 0 { 26 } else { 24 } + if frame_control_1 & 0x80 != 0 { 4 } else { 0 };
    let decrypt_status = frame.get(36).map_or(0, |status| (status >> 2) & 0x07);
    let is_80211_payload = frame.get(48).is_some_and(|flags| flags & (1 << 1) != 0);
    if is_80211_payload {
        return Ok(None);
    }
    let crypto_header_len = match decrypt_status {
        1 => 4,
        2..=6 => 8,
        7 => 18,
        _ => 0,
    };
    let llc_offset = header_len + crypto_header_len;
    let ether_type_offset = llc_offset + 6;
    let payload_offset = llc_offset + 8;
    if payload_offset > mpdu.len() || ether_type_offset + 2 > mpdu.len() {
        return Err(DataWireError::TruncatedAggregate);
    }

    let (destination, source) = ieee80211_addresses(mpdu, frame_control_1)?;
    let mut ethernet = Vec::with_capacity(ETHERNET_HEADER_BYTES + mpdu.len() - payload_offset);
    ethernet.extend_from_slice(destination);
    ethernet.extend_from_slice(source);
    ethernet.extend_from_slice(&mpdu[ether_type_offset..ether_type_offset + 2]);
    ethernet.extend_from_slice(&mpdu[payload_offset..]);
    Ok(Some(ethernet))
}

fn ieee80211_addresses(mpdu: &[u8], frame_control_1: u8) -> Result<(&[u8], &[u8]), DataWireError> {
    let to_ds = frame_control_1 & 0x01 != 0;
    let from_ds = frame_control_1 & 0x02 != 0;
    let pair = match (to_ds, from_ds) {
        (false, false) => (4..10, 10..16),
        (true, false) => (16..22, 10..16),
        (false, true) => (4..10, 16..22),
        (true, true) => (16..22, 24..30),
    };
    let destination = mpdu.get(pair.0).ok_or(DataWireError::TruncatedAggregate)?;
    let source = mpdu.get(pair.1).ok_or(DataWireError::TruncatedAggregate)?;
    Ok((destination, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESTINATION: [u8; 6] = [0x02, 1, 2, 3, 4, 5];
    const SOURCE: [u8; 6] = [0x02, 6, 7, 8, 9, 10];

    fn ethernet_packet() -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&DESTINATION);
        packet.extend_from_slice(&SOURCE);
        packet.extend_from_slice(&[0x08, 0x00]);
        packet.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        packet
    }

    #[test]
    fn tx_frame_preserves_ethernet_routing_and_payload() {
        let ethernet = ethernet_packet();
        let frame = build_tx_frame(ChipVariant::Aic8800DC, &ethernet, 3, 7).unwrap();

        assert_eq!(frame.len(), crate::wire::SDIO_BLOCK_SIZE);
        assert_eq!(frame[2], SDIO_TYPE_DATA);
        assert_eq!(&frame[12..18], &DESTINATION);
        assert_eq!(&frame[18..24], &SOURCE);
        assert_eq!(&frame[24..26], &[0x08, 0x00]);
        assert_eq!(frame[28], 3);
        assert_eq!(frame[29], 7);
        assert_eq!(&frame[32..36], &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn rx_aggregate_converts_from_ds_80211_packet_to_ethernet() {
        let ethernet = ethernet_packet();
        let mut mpdu = vec![0; 24];
        mpdu[0] = 0x08;
        mpdu[1] = 0x02;
        mpdu[4..10].copy_from_slice(&DESTINATION);
        mpdu[16..22].copy_from_slice(&SOURCE);
        mpdu.extend_from_slice(&[0xaa, 0xaa, 0x03, 0, 0, 0, 0x08, 0x00]);
        mpdu.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);

        let mut aggregate = vec![0; RX_HARDWARE_HEADER_BYTES + mpdu.len()];
        aggregate[..2].copy_from_slice(&(mpdu.len() as u16).to_le_bytes());
        aggregate[2] = SDIO_TYPE_DATA;
        aggregate[RX_HARDWARE_HEADER_BYTES..].copy_from_slice(&mpdu);
        aggregate.resize(aggregate.len().next_multiple_of(RX_ALIGNMENT), 0);

        assert_eq!(decode_rx_aggregate(&aggregate).unwrap(), vec![ethernet]);
    }
}
