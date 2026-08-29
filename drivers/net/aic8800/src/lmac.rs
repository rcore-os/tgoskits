//! Typed AIC LMAC message construction and confirmation parsing.
//!
//! Layouts here follow the vendor Linux AIC8800 driver. Device ownership and
//! state transitions deliberately live outside this wire-format module.

use alloc::{vec, vec::Vec};

use crate::device::AicError;

pub(crate) const TASK_MM: u16 = 0;
pub(crate) const TASK_ME: u16 = 5;
pub(crate) const TASK_SM: u16 = 6;

pub(crate) const MM_RESET_REQ: u16 = 0x0000;
pub(crate) const MM_RESET_CFM: u16 = 0x0001;
pub(crate) const MM_START_REQ: u16 = 0x0002;
pub(crate) const MM_START_CFM: u16 = 0x0003;
pub(crate) const MM_ADD_IF_REQ: u16 = 0x0006;
pub(crate) const MM_ADD_IF_CFM: u16 = 0x0007;
pub(crate) const MM_SET_FILTER_REQ: u16 = 0x000e;
pub(crate) const MM_SET_FILTER_CFM: u16 = 0x000f;
pub(crate) const MM_KEY_ADD_REQ: u16 = 0x0024;
pub(crate) const MM_KEY_ADD_CFM: u16 = 0x0025;
pub(crate) const MM_SET_RF_CALIB_REQ: u16 = 0x0069;
pub(crate) const MM_SET_RF_CALIB_CFM: u16 = 0x006a;
pub(crate) const MM_SET_RF_CONFIG_REQ: u16 = 0x0067;
pub(crate) const MM_SET_RF_CONFIG_CFM: u16 = 0x0068;
pub(crate) const MM_GET_MAC_ADDR_REQ: u16 = 0x0073;
pub(crate) const MM_GET_MAC_ADDR_CFM: u16 = 0x0074;
pub(crate) const MM_SET_STACK_START_REQ: u16 = 0x007b;
pub(crate) const MM_SET_STACK_START_CFM: u16 = 0x007c;
// Unsolicited MM indications may be interleaved with control confirmations
// while the firmware is associating.  They share the CFG_CMD_RSP transport
// type, so the receive parser needs the protocol classification rather than
// treating every non-SM message as a mailbox confirmation.
pub(crate) const MM_PRIMARY_TBTT_IND: u16 = 0x002c;
pub(crate) const MM_SECONDARY_TBTT_IND: u16 = 0x002d;
pub(crate) const MM_CONNECTION_LOSS_IND: u16 = 0x0043;
pub(crate) const MM_CHANNEL_SWITCH_IND: u16 = 0x0044;
pub(crate) const MM_CHANNEL_PRE_SWITCH_IND: u16 = 0x0045;
pub(crate) const MM_REMAIN_ON_CHANNEL_EXP_IND: u16 = 0x0048;
pub(crate) const MM_PS_CHANGE_IND: u16 = 0x0049;
pub(crate) const MM_TRAFFIC_REQ_IND: u16 = 0x004a;
pub(crate) const MM_P2P_VIF_PS_CHANGE_IND: u16 = 0x004d;
pub(crate) const MM_CSA_COUNTER_IND: u16 = 0x004e;
pub(crate) const MM_CHANNEL_SURVEY_IND: u16 = 0x004f;
pub(crate) const MM_P2P_NOA_UPD_IND: u16 = 0x0055;
pub(crate) const MM_RSSI_STATUS_IND: u16 = 0x0057;
pub(crate) const MM_CSA_FINISH_IND: u16 = 0x0058;
pub(crate) const MM_CSA_TRAFFIC_IND: u16 = 0x0059;
pub(crate) const MM_PKTLOSS_IND: u16 = 0x0060;
pub(crate) const MM_APM_STALOSS_IND: u16 = 0x007d;
pub(crate) const MM_RADAR_DETECT_IND: u16 = 0x008b;
pub(crate) const MM_SET_TXPWR_IDX_LVL_REQ: u16 = 0x0077;
pub(crate) const MM_SET_TXPWR_IDX_LVL_CFM: u16 = 0x0078;
pub(crate) const ME_CONFIG_REQ: u16 = 0x1400;
pub(crate) const ME_CONFIG_CFM: u16 = 0x1401;
pub(crate) const ME_CHAN_CONFIG_REQ: u16 = 0x1402;
pub(crate) const ME_CHAN_CONFIG_CFM: u16 = 0x1403;
pub(crate) const ME_SET_CONTROL_PORT_REQ: u16 = 0x1404;
pub(crate) const ME_SET_CONTROL_PORT_CFM: u16 = 0x1405;
// The Linux driver may issue this request after a TX queue transition.  The
// firmware can return its confirmation asynchronously even when this Rust
// owner did not submit the optional traffic indication request, so it must not
// be mistaken for the confirmation of the active control mailbox.
pub(crate) const ME_TRAFFIC_IND_CFM: u16 = 0x140b;
pub(crate) const SM_CONNECT_REQ: u16 = 0x1800;
pub(crate) const SM_CONNECT_CFM: u16 = 0x1801;
pub(crate) const SM_CONNECT_IND: u16 = 0x1802;
pub(crate) const SM_DISCONNECT_REQ: u16 = 0x1803;
pub(crate) const SM_DISCONNECT_CFM: u16 = 0x1804;
pub(crate) const SM_DISCONNECT_IND: u16 = 0x1805;
// SCANU is the firmware's full-MAC scan task (task id 4, base 0x1000).
// Result frames are unsolicited and can arrive while a station request is
// being staged, so they must not be mistaken for a mailbox confirmation.
pub(crate) const SCANU_RESULT_IND: u16 = 0x1004;

pub(crate) const RSN_IE_CCMP_PSK: [u8; 22] = [
    0x30, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2, 0, 0,
];

// `struct sm_connect_req` from the Linux AIC8800 driver is sent with the
// compiler's native C alignment.  In particular, `mac_addr` starts after the
// 33-byte SSID field, and `mac_chan_def` is six bytes (including its trailing
// two-byte alignment).  Keep the offsets in one place so the payload builder
// cannot silently drift when fields are added elsewhere.
const SM_CONNECT_PAYLOAD_LEN: usize = 320;
const SM_CONNECT_BSSID_OFFSET: usize = 34;
const SM_CONNECT_CHANNEL_OFFSET: usize = 40;
const SM_CONNECT_FLAGS_OFFSET: usize = 48;
const SM_CONNECT_CONTROL_PORT_OFFSET: usize = 52;
const SM_CONNECT_IE_LEN_OFFSET: usize = 54;
const SM_CONNECT_VIF_OFFSET: usize = 61;
const SM_CONNECT_IE_OFFSET: usize = 64;

pub(crate) struct ConnectIndication {
    pub(crate) bssid: [u8; 6],
    pub(crate) interface_index: u8,
    pub(crate) station_index: u8,
}

pub(crate) struct DisconnectIndication {
    pub(crate) reason_code: u16,
    pub(crate) interface_index: u8,
}

pub(crate) const fn is_indication_message(message_id: u16) -> bool {
    matches!(
        message_id,
        MM_PRIMARY_TBTT_IND
            | MM_SECONDARY_TBTT_IND
            | MM_CONNECTION_LOSS_IND
            | MM_CHANNEL_SWITCH_IND
            | MM_CHANNEL_PRE_SWITCH_IND
            | MM_REMAIN_ON_CHANNEL_EXP_IND
            | MM_PS_CHANGE_IND
            | MM_TRAFFIC_REQ_IND
            | MM_P2P_VIF_PS_CHANGE_IND
            | MM_CSA_COUNTER_IND
            | MM_CHANNEL_SURVEY_IND
            | MM_P2P_NOA_UPD_IND
            | MM_RSSI_STATUS_IND
            | MM_CSA_FINISH_IND
            | MM_CSA_TRAFFIC_IND
            | MM_PKTLOSS_IND
            | MM_APM_STALOSS_IND
            | MM_RADAR_DETECT_IND
            | ME_TRAFFIC_IND_CFM
            | SCANU_RESULT_IND
            | SM_CONNECT_IND
            | SM_DISCONNECT_IND
    )
}

pub(crate) fn require_empty(_message_id: u16, payload: &[u8]) -> Result<(), AicError> {
    if payload.is_empty() {
        Ok(())
    } else {
        Err(AicError::MalformedResponse)
    }
}

pub(crate) fn require_status_ok(message_id: u16, payload: &[u8]) -> Result<(), AicError> {
    let status = *payload.first().ok_or(AicError::MalformedResponse)?;
    if status == 0 {
        Ok(())
    } else {
        Err(AicError::FirmwareRejected {
            message_id,
            status: u16::from(status),
        })
    }
}

pub(crate) fn parse_mac(payload: &[u8]) -> Result<[u8; 6], AicError> {
    payload.try_into().map_err(|_| AicError::MalformedResponse)
}

pub(crate) fn parse_add_interface(payload: &[u8]) -> Result<u8, AicError> {
    if payload.len() != 2 {
        return Err(AicError::MalformedResponse);
    }
    require_status_ok(MM_ADD_IF_CFM, payload)?;
    (payload[1] != u8::MAX)
        .then_some(payload[1])
        .ok_or(AicError::MalformedResponse)
}

pub(crate) fn parse_connect_indication(payload: &[u8]) -> Result<ConnectIndication, AicError> {
    if payload.len() < 11 {
        return Err(AicError::MalformedResponse);
    }
    let status = u16::from_le_bytes([payload[0], payload[1]]);
    if status != 0 {
        return Err(AicError::FirmwareRejected {
            message_id: SM_CONNECT_IND,
            status,
        });
    }
    Ok(ConnectIndication {
        bssid: payload[2..8]
            .try_into()
            .map_err(|_| AicError::MalformedResponse)?,
        interface_index: payload[9],
        station_index: payload[10],
    })
}

pub(crate) fn parse_disconnect_indication(
    payload: &[u8],
) -> Result<DisconnectIndication, AicError> {
    if !matches!(payload.len(), 5 | 6) || payload.get(5).is_some_and(|padding| *padding != 0) {
        return Err(AicError::MalformedResponse);
    }
    let interface_index = payload[2];
    if interface_index == u8::MAX {
        return Err(AicError::MalformedResponse);
    }
    Ok(DisconnectIndication {
        reason_code: u16::from_le_bytes([payload[0], payload[1]]),
        interface_index,
    })
}

pub(crate) const fn stack_start_payload(vendor: u8) -> [u8; 4] {
    [1, 0, vendor, 0]
}

pub(crate) fn tx_power_level_payload() -> [u8; 95] {
    let mut payload = [0; 95];
    let profiles: [&[u8]; 6] = [
        &[20, 20, 20, 20, 20, 20, 20, 20, 18, 18, 16, 16],
        &[20, 20, 20, 20, 18, 18, 16, 16, 16, 16],
        &[20, 20, 20, 20, 18, 18, 16, 16, 16, 16, 15, 15],
        &[0x80, 0x80, 0x80, 0x80, 20, 20, 20, 20, 18, 18, 16, 16],
        &[20, 20, 20, 20, 18, 18, 16, 16, 16, 15],
        &[20, 20, 20, 20, 18, 18, 16, 16, 16, 15, 14, 14],
    ];
    payload[0] = 1;
    let mut offset = 1;
    for profile in profiles {
        payload[offset..offset + profile.len()].copy_from_slice(profile);
        offset += profile.len();
    }
    payload
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RfCalibrationBand {
    Ghz2Only,
    DualBand,
}

pub(crate) fn rf_calibration_payload(band: RfCalibrationBand) -> [u8; 24] {
    let mut payload = [0; 24];
    payload[0..4].copy_from_slice(&0x0000_0f8fu32.to_le_bytes());
    if band == RfCalibrationBand::DualBand {
        payload[4..8].copy_from_slice(&0x0000_0f0fu32.to_le_bytes());
    }
    payload[8..12].copy_from_slice(&0x0c34_c008u32.to_le_bytes());
    payload[16..20].copy_from_slice(&0x0026_4203u32.to_le_bytes());
    payload
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RfTableSelection {
    Receive  = 0,
    Transmit = 1,
}

pub(crate) fn rf_config_payload(
    selection: RfTableSelection,
    table_offset: u8,
    words: &[u32],
) -> Result<[u8; 260], AicError> {
    if words.len() > 64 {
        return Err(AicError::InvalidFirmwareAsset);
    }
    let mut payload = [0; 260];
    payload[0] = selection as u8;
    payload[1] = table_offset;
    payload[2] = 16;
    for (index, word) in words.iter().enumerate() {
        let offset = 4 + index * 4;
        payload[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    Ok(payload)
}

pub(crate) const fn get_mac_payload() -> [u8; 4] {
    1u32.to_le_bytes()
}

pub(crate) fn me_config_payload() -> [u8; 112] {
    let mut payload = [0; 112];
    // struct mac_htcapability starts at offset 0. Enable LDPC, advertise the
    // vendor AMPDU limits, and use the first MCS byte for MCS 0 through 7.
    payload[0..2].copy_from_slice(&1u16.to_le_bytes());
    payload[2] = 3 | (7 << 2);
    payload[3] = 0xff;
    payload[13..15].copy_from_slice(&65u16.to_le_bytes());
    payload[15] = 1;

    // struct me_config_req places these scalar fields after HT/VHT/HE
    // capability structures, including the C ABI padding between them.
    payload[100..102].copy_from_slice(&1000u16.to_le_bytes());
    payload[102] = 2; // PHY_CHNL_BW_80
    payload[103] = 1; // HT supported
    payload[107] = 1; // power-save enabled
    payload
}

pub(crate) fn channel_config_payload() -> [u8; 254] {
    let mut payload = [0; 254];
    const CHANNELS: [u16; 14] = [
        2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462, 2467, 2472, 2484,
    ];
    for (index, frequency) in CHANNELS.into_iter().enumerate() {
        let offset = index * 6;
        payload[offset..offset + 2].copy_from_slice(&frequency.to_le_bytes());
        payload[offset + 4] = 30;
    }
    payload[252] = CHANNELS.len() as u8;
    payload
}

pub(crate) fn add_interface_payload(mac: [u8; 6], role: u8) -> [u8; 10] {
    let mut payload = [0; 10];
    payload[0] = role;
    payload[2..8].copy_from_slice(&mac);
    payload
}

pub(crate) fn connect_payload(ssid: &[u8], secured: bool, interface_index: u8) -> Vec<u8> {
    let mut payload = vec![0; SM_CONNECT_PAYLOAD_LEN];
    payload[0] = ssid.len() as u8;
    payload[1..1 + ssid.len()].copy_from_slice(ssid);
    payload[SM_CONNECT_BSSID_OFFSET..SM_CONNECT_BSSID_OFFSET + 6].fill(0xff);
    payload[SM_CONNECT_CHANNEL_OFFSET..SM_CONNECT_CHANNEL_OFFSET + 2]
        .copy_from_slice(&0xffffu16.to_le_bytes());
    if secured {
        // CONTROL_PORT_HOST | CONTROL_PORT_NO_ENC | WPA_WPA2.
        payload[SM_CONNECT_FLAGS_OFFSET..SM_CONNECT_FLAGS_OFFSET + 4]
            .copy_from_slice(&0x0000_000bu32.to_le_bytes());
        payload[SM_CONNECT_IE_OFFSET..SM_CONNECT_IE_OFFSET + RSN_IE_CCMP_PSK.len()]
            .copy_from_slice(&RSN_IE_CCMP_PSK);
        payload[SM_CONNECT_IE_LEN_OFFSET..SM_CONNECT_IE_LEN_OFFSET + 2]
            .copy_from_slice(&(RSN_IE_CCMP_PSK.len() as u16).to_le_bytes());
    }
    payload[SM_CONNECT_CONTROL_PORT_OFFSET..SM_CONNECT_CONTROL_PORT_OFFSET + 2]
        .copy_from_slice(&0x888eu16.to_be_bytes());
    payload[SM_CONNECT_VIF_OFFSET] = interface_index;
    payload
}

pub(crate) const fn control_port_payload(station_index: u8, open: bool) -> [u8; 2] {
    [station_index, open as u8]
}

pub(crate) fn disconnect_payload(interface_index: u8) -> [u8; 4] {
    [3, 0, interface_index, 0]
}

pub(crate) fn key_add_payload(
    interface_index: u8,
    station_index: u8,
    pairwise: bool,
    key_index: u8,
    key: &[u8],
) -> Result<[u8; 44], AicError> {
    if key.len() != 16 {
        return Err(AicError::WpaKeyData);
    }
    let mut payload = [0; 44];
    payload[0] = key_index;
    payload[1] = station_index;
    payload[4] = key.len() as u8;
    payload[8..8 + key.len()].copy_from_slice(key);
    payload[40] = 2; // MAC_CIPHER_CCMP
    payload[41] = interface_index;
    payload[43] = pairwise as u8;
    Ok(payload)
}

pub(crate) fn parse_key_add_confirmation(payload: &[u8]) -> Result<u8, AicError> {
    // Linux's `struct mm_key_add_cfm` is `{ u8 status; u8 hw_key_idx; }`.
    // The firmware emits exactly these two bytes; accepting a padded variant
    // would hide a transport/layout mismatch.
    if payload.len() != 2 {
        return Err(AicError::MalformedResponse);
    }
    require_status_ok(MM_KEY_ADD_CFM, payload)?;
    (payload[1] != u8::MAX)
        .then_some(payload[1])
        .ok_or(AicError::MalformedResponse)
}

pub(crate) const fn filter_payload() -> [u8; 4] {
    0x1502_868cu32.to_le_bytes()
}

pub(crate) const fn start_payload() -> [u8; 72] {
    let mut payload = [0; 72];
    let timeout = 300u32.to_le_bytes();
    let clock_accuracy = 20u16.to_le_bytes();
    payload[64] = timeout[0];
    payload[65] = timeout[1];
    payload[66] = timeout[2];
    payload[67] = timeout[3];
    payload[68] = clock_accuracy[0];
    payload[69] = clock_accuracy[1];
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_confirmation_status_is_not_an_association_result() {
        assert_eq!(require_status_ok(SM_CONNECT_CFM, &[0]), Ok(()));
        assert_eq!(
            require_status_ok(SM_CONNECT_CFM, &[7]),
            Err(AicError::FirmwareRejected {
                message_id: SM_CONNECT_CFM,
                status: 7,
            })
        );
    }

    #[test]
    fn add_interface_rejects_invalid_firmware_index() {
        assert_eq!(
            parse_add_interface(&[0, u8::MAX]),
            Err(AicError::MalformedResponse)
        );
    }

    #[test]
    fn d80_rf_calibration_matches_the_vendor_request() {
        let payload = rf_calibration_payload(RfCalibrationBand::DualBand);

        assert_eq!(payload.len(), 24);
        assert_eq!(&payload[0..4], &0x0000_0f8fu32.to_le_bytes());
        assert_eq!(&payload[4..8], &0x0000_0f0fu32.to_le_bytes());
        assert_eq!(&payload[8..12], &0x0c34_c008u32.to_le_bytes());
        assert_eq!(&payload[12..16], &0u32.to_le_bytes());
        assert_eq!(&payload[16..20], &0x0026_4203u32.to_le_bytes());
        assert_eq!(&payload[20..24], &[0; 4]);
    }

    #[test]
    fn dc_rf_calibration_matches_the_2ghz_only_vendor_request() {
        let payload = rf_calibration_payload(RfCalibrationBand::Ghz2Only);

        assert_eq!(&payload[0..4], &0x0000_0f8fu32.to_le_bytes());
        assert_eq!(&payload[4..8], &0u32.to_le_bytes());
    }

    #[test]
    fn dc_rf_config_uses_the_vendor_c_layout() {
        let payload = rf_config_payload(RfTableSelection::Transmit, 16, &[0x1122_3344]).unwrap();

        assert_eq!(&payload[..4], &[1, 16, 16, 0]);
        assert_eq!(&payload[4..8], &0x1122_3344u32.to_le_bytes());
        assert_eq!(&payload[8..], &[0; 252]);
    }

    #[test]
    fn secured_connect_uses_the_vendor_sm_connect_layout() {
        let payload = connect_payload(b"network", true, 6);

        assert_eq!(payload.len(), 320);
        assert_eq!(payload[33], 0);
        assert_eq!(&payload[34..40], &[0xff; 6]);
        assert_eq!(&payload[40..42], &0xffffu16.to_le_bytes());
        assert_eq!(&payload[48..52], &0x0000_000bu32.to_le_bytes());
        assert_eq!(&payload[52..54], &0x888eu16.to_be_bytes());
        assert_eq!(
            &payload[54..56],
            &(RSN_IE_CCMP_PSK.len() as u16).to_le_bytes()
        );
        assert_eq!(&payload[56..61], &[0; 5]);
        assert_eq!(payload[61], 6);
        assert_eq!(&payload[62..64], &[0; 2]);
        assert_eq!(&payload[64..64 + RSN_IE_CCMP_PSK.len()], &RSN_IE_CCMP_PSK);
    }

    #[test]
    fn mac_start_uses_the_vendor_runtime_defaults() {
        let payload = start_payload();

        assert_eq!(payload.len(), 72);
        assert_eq!(&payload[..64], &[0; 64]);
        assert_eq!(&payload[64..68], &300u32.to_le_bytes());
        assert_eq!(&payload[68..70], &20u16.to_le_bytes());
        assert_eq!(&payload[70..72], &[0; 2]);
    }

    #[test]
    fn key_add_uses_the_vendor_mm_key_add_layout() {
        let key = [0x5a; 16];
        let payload = key_add_payload(2, 7, true, 0, &key).unwrap();

        assert_eq!(payload.len(), 44);
        assert_eq!(payload[0], 0);
        assert_eq!(payload[1], 7);
        assert_eq!(payload[4], key.len() as u8);
        assert_eq!(&payload[8..24], &key);
        assert_eq!(payload[40], 2);
        assert_eq!(payload[41], 2);
        assert_eq!(payload[42], 0);
        assert_eq!(payload[43], 1);
    }

    #[test]
    fn disconnect_message_ids_follow_the_vendor_sm_enum() {
        assert_eq!(SM_DISCONNECT_REQ, 0x1803);
        assert_eq!(SM_DISCONNECT_CFM, 0x1804);
        assert_eq!(SM_DISCONNECT_IND, 0x1805);
    }

    #[test]
    fn d80_tx_power_profile_matches_the_vendor_defaults() {
        let payload = tx_power_level_payload();

        assert_eq!(payload.len(), 95);
        assert_eq!(payload[0], 1);
        assert_eq!(
            &payload[1..13],
            &[20, 20, 20, 20, 20, 20, 20, 20, 18, 18, 16, 16]
        );
        assert_eq!(
            &payload[35..47],
            &[0x80, 0x80, 0x80, 0x80, 20, 20, 20, 20, 18, 18, 16, 16]
        );
        assert_eq!(&payload[69..], &[0; 26]);
    }

    #[test]
    fn mac_capability_config_uses_the_vendor_c_layout() {
        let payload = me_config_payload();

        assert_eq!(payload.len(), 112);
        assert_eq!(&payload[0..2], &1u16.to_le_bytes());
        assert_eq!(payload[2], 31);
        assert_eq!(payload[3], 0xff);
        assert_eq!(&payload[13..15], &65u16.to_le_bytes());
        assert_eq!(payload[15], 1);
        assert_eq!(&payload[100..102], &1000u16.to_le_bytes());
        assert_eq!(payload[102], 2);
        assert_eq!(payload[103], 1);
        assert_eq!(payload[107], 1);
        assert_eq!(&payload[110..112], &[0; 2]);
    }

    #[test]
    fn channel_config_uses_six_byte_vendor_channel_entries() {
        let payload = channel_config_payload();

        assert_eq!(payload.len(), 254);
        assert_eq!(&payload[0..2], &2412u16.to_le_bytes());
        assert_eq!(&payload[6..8], &2417u16.to_le_bytes());
        assert_eq!(payload[4], 30);
        assert_eq!(payload[252], 14);
        assert_eq!(payload[253], 0);
    }

    #[test]
    fn disconnect_request_and_key_confirmation_use_exact_vendor_sizes() {
        let disconnect = disconnect_payload(6);
        assert_eq!(disconnect.len(), 4);
        assert_eq!(disconnect.as_slice(), &[3, 0, 6, 0]);
        assert_eq!(parse_key_add_confirmation(&[0, 1]), Ok(1));
        assert_eq!(
            parse_key_add_confirmation(&[0, 1, 0, 0]),
            Err(AicError::MalformedResponse)
        );
    }

    #[test]
    fn asynchronous_traffic_confirmation_is_not_a_control_mailbox_result() {
        assert!(is_indication_message(ME_TRAFFIC_IND_CFM));
        assert!(!is_indication_message(ME_SET_CONTROL_PORT_CFM));
    }
}
