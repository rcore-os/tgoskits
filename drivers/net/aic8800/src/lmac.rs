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
pub(crate) const APM_START_REQ: u16 = 0x1c00;
pub(crate) const APM_START_CFM: u16 = 0x1c01;
pub(crate) const APM_SET_BEACON_IE_REQ: u16 = 0x1c08;
pub(crate) const APM_SET_BEACON_IE_CFM: u16 = 0x1c09;
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
// 0x140b is the firmware's own credit update: the vendor's handler for it is
// `rwnx_rx_me_tx_credits_update_ind`, and this driver takes its credit from the
// flow-control register instead, so the message is only classified.
pub(crate) const ME_TX_CREDITS_UPDATE_IND: u16 = 0x140b;
// The Linux driver may issue this request after a TX queue transition, so the
// firmware can return its confirmation asynchronously even when this Rust
// owner did not submit the optional traffic indication request.  The value is
// that message's index in the vendor's LMAC message table, and it must not be
// mistaken for the confirmation of the active control mailbox.
pub(crate) const ME_TRAFFIC_IND_CFM: u16 = 0x140d;
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
            | ME_TX_CREDITS_UPDATE_IND
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

pub(crate) fn parse_ap_start(payload: &[u8], interface_index: u8) -> Result<(), AicError> {
    if payload.len() != 4 {
        return Err(AicError::MalformedResponse);
    }
    require_status_ok(APM_START_CFM, payload)?;
    if payload[1] != interface_index || payload[2] == u8::MAX || payload[3] == u8::MAX {
        return Err(AicError::MalformedResponse);
    }
    Ok(())
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

const ME_CONFIG_PAYLOAD_LEN: usize = 112;
const ME_CONFIG_HT_OFFSET: usize = 0;
const ME_CONFIG_VHT_OFFSET: usize = 32;
const ME_CONFIG_HE_OFFSET: usize = 44;
const ME_CONFIG_TX_LIFETIME_OFFSET: usize = 100;
const ME_CONFIG_PHY_BW_OFFSET: usize = 102;
/// PHY bandwidth the ME configuration declares. The vendor forces 80 MHz on
/// D80 and derives the value on DC from the RF bandwidth the firmware reports;
/// this driver does not read the RF feature register, so every profile declares
/// the vendor's forced value.
const ME_CONFIG_PHY_BW_MAX: u8 = 2;
const ME_CONFIG_HT_SUPPORTED_OFFSET: usize = 103;
const ME_CONFIG_VHT_SUPPORTED_OFFSET: usize = 104;
const ME_CONFIG_HE_SUPPORTED_OFFSET: usize = 105;
const ME_CONFIG_HE_UL_ON_OFFSET: usize = 106;
const ME_CONFIG_PS_ON_OFFSET: usize = 107;
const ME_CONFIG_ANT_DIV_ON_OFFSET: usize = 108;
const ME_CONFIG_DPSM_OFFSET: usize = 109;

const HT_CAPABILITY_INFO_OFFSET: usize = ME_CONFIG_HT_OFFSET;
const HT_AMPDU_PARAM_OFFSET: usize = ME_CONFIG_HT_OFFSET + 2;
const HT_MCS_OFFSET: usize = ME_CONFIG_HT_OFFSET + 3;
const HT_MCS_RX_MASK_LEN: usize = 10;
/// Whole HT MCS field: the rx mask, the rx highest rate, the tx parameters and
/// the reserved tail the vendor leaves zero.
const HT_MCS_LEN: usize = 16;
const HT_MCS_RX_HIGHEST_OFFSET: usize = HT_MCS_OFFSET + HT_MCS_RX_MASK_LEN;
const HT_MCS_TX_PARAMS_OFFSET: usize = HT_MCS_RX_HIGHEST_OFFSET + 2;
const HT_MCS_RESERVED_OFFSET: usize = HT_MCS_TX_PARAMS_OFFSET + 1;
const HT_CAP_LDPC: u16 = 0x0001;
const HT_CAP_WIDTH_20_40: u16 = 0x0002;
const HT_CAP_SGI_20: u16 = 0x0020;
const HT_CAP_SGI_40: u16 = 0x0040;
const HT_CAP_MAX_AMSDU: u16 = 0x0800;
const HT_MCS_TX_DEFINED: u8 = 0x01;

const VHT_CAPABILITY_INFO_OFFSET: usize = ME_CONFIG_VHT_OFFSET;
const VHT_RX_MCS_MAP_OFFSET: usize = ME_CONFIG_VHT_OFFSET + 4;
const VHT_RX_HIGHEST_OFFSET: usize = ME_CONFIG_VHT_OFFSET + 6;
const VHT_TX_MCS_MAP_OFFSET: usize = ME_CONFIG_VHT_OFFSET + 8;
const VHT_TX_HIGHEST_OFFSET: usize = ME_CONFIG_VHT_OFFSET + 10;

const VHT_CAP_MAX_MPDU_LENGTH_7991: u32 = 0x0000_0001;
const VHT_CAP_RXLDPC: u32 = 0x0000_0010;
const VHT_CAP_RXSTBC_1: u32 = 0x0000_0100;
const VHT_CAP_SU_BEAMFORMEE_CAPABLE: u32 = 0x0000_1000;
const VHT_CAP_BEAMFORMEE_STS_3: u32 = 3 << 13;
const VHT_CAP_MU_BEAMFORMER_CAPABLE: u32 = 0x0008_0000;
const VHT_CAP_MU_BEAMFORMEE_CAPABLE: u32 = 0x0010_0000;
const VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_7: u32 = 7 << 23;
const VHT_MCS_SUPPORT_0_9: u16 = 2;

/// Highest rate the vendor's rate table records for a single stream supporting
/// MCS 0-9, in the 1 Mbit/s units of the VHT MCS map.
const VHT_HIGHEST_RATE_1SS_MCS_0_9: u16 = 390;

const HE_MAC_CAP_INFO_LEN: usize = 6;
const HE_PHY_CAP_INFO_LEN: usize = 11;
const HE_PPE_THRESHOLD_LEN: usize = 25;
const HE_MAC_CAP_INFO_OFFSET: usize = ME_CONFIG_HE_OFFSET;
const HE_PHY_CAP_INFO_OFFSET: usize = ME_CONFIG_HE_OFFSET + HE_MAC_CAP_INFO_LEN;
const HE_MCS_PADDING_OFFSET: usize = HE_PHY_CAP_INFO_OFFSET + HE_PHY_CAP_INFO_LEN;
const HE_MCS_SUPPORT_OFFSET: usize = HE_MCS_PADDING_OFFSET + 1;
const HE_PPE_THRESHOLDS_OFFSET: usize = HE_MCS_SUPPORT_OFFSET + 12;

const HE_MAC_CAP2_ALL_ACK: u8 = 0x02;
const HE_PHY_CAP0_WIDTH_40MHZ_IN_2G: u8 = 0x02;
const HE_PHY_CAP0_WIDTH_40MHZ_80MHZ_IN_5G: u8 = 0x04;
const HE_PHY_CAP1_LDPC_CODING_IN_PAYLOAD: u8 = 0x20;
const HE_PHY_CAP1_HE_LTF_AND_GI_FOR_HE_PPDUS_0_8US: u8 = 0x40;
const HE_PHY_CAP1_MIDAMBLE_RX_TX_MAX_NSTS: u8 = 0x80;
const HE_PHY_CAP2_MIDAMBLE_RX_TX_MAX_NSTS: u8 = 0x01;
const HE_PHY_CAP2_NDP_4X_LTF_AND_3_2US: u8 = 0x02;
const HE_PHY_CAP2_STBC_RX_UNDER_80MHZ: u8 = 0x08;
const HE_PHY_CAP2_DOPPLER_RX: u8 = 0x20;
const HE_PHY_CAP3_DCM_MAX_CONST_RX_16_QAM: u8 = 0x18;
const HE_PHY_CAP3_RX_HE_MU_PPDU_FROM_NON_AP_STA: u8 = 0x40;
const HE_PHY_CAP4_SU_BEAMFORMEE: u8 = 0x01;
const HE_PHY_CAP4_BEAMFORMEE_MAX_STS_UNDER_80MHZ_4: u8 = 0x0c;
const HE_PHY_CAP5_NG16_SU_FEEDBACK: u8 = 0x40;
const HE_PHY_CAP5_NG16_MU_FEEDBACK: u8 = 0x80;
const HE_PHY_CAP6_CODEBOOK_SIZE_42_SU: u8 = 0x01;
const HE_PHY_CAP6_CODEBOOK_SIZE_75_MU: u8 = 0x02;
const HE_PHY_CAP6_TRIG_SU_BEAMFORMER_FB: u8 = 0x04;
const HE_PHY_CAP6_TRIG_MU_BEAMFORMER_FB: u8 = 0x08;
const HE_PHY_CAP6_PARTIAL_BANDWIDTH_DL_MUMIMO: u8 = 0x40;
const HE_PHY_CAP6_PPE_THRESHOLD_PRESENT: u8 = 0x80;
const HE_PHY_CAP8_20MHZ_IN_40MHZ_HE_PPDU_IN_2G: u8 = 0x02;
const HE_PHY_CAP9_RX_FULL_BW_SU_USING_MU_WITH_COMP_SIGB: u8 = 0x10;
const HE_PHY_CAP9_RX_FULL_BW_SU_USING_MU_WITH_NON_COMP_SIGB: u8 = 0x20;
const HE_MCS_SUPPORT_0_11: u16 = 2;

/// Base PPE threshold bytes and the markers the vendor adds for 40 MHz and
/// 80 MHz operation.
const HE_PPE_THRESHOLD_BASE_0: u8 = 0x08;
const HE_PPE_THRESHOLD_BASE_1: u8 = 0x1c;
const HE_PPE_THRESHOLD_BASE_2: u8 = 0x07;
const HE_PPE_THRESHOLD_40MHZ_MARKER_0: u8 = 0x10;
const HE_PPE_THRESHOLD_80MHZ_MARKER_0: u8 = 0x20;
const HE_PPE_THRESHOLD_80MHZ_MARKER_2: u8 = 0xc0;
const HE_PPE_THRESHOLD_80MHZ_MARKER_3: u8 = 0x01;

/// Both the VHT and the HE MCS map encode "this number of streams is not
/// supported" as three in each two-bit field.
const MCS_MAP_NOT_SUPPORTED: u16 = 0x3;

/// MCS map of a single stream that supports `supported_mcs`: every remaining
/// stream count is marked as unsupported.
const fn single_stream_mcs_map(supported_mcs: u16) -> u16 {
    supported_mcs
        | (MCS_MAP_NOT_SUPPORTED << 2)
        | (MCS_MAP_NOT_SUPPORTED << 4)
        | (MCS_MAP_NOT_SUPPORTED << 6)
        | (MCS_MAP_NOT_SUPPORTED << 8)
        | (MCS_MAP_NOT_SUPPORTED << 10)
        | (MCS_MAP_NOT_SUPPORTED << 12)
        | (MCS_MAP_NOT_SUPPORTED << 14)
}

/// MCS map that marks every stream count as unsupported.
const UNSUPPORTED_MCS_MAP: u16 = single_stream_mcs_map(MCS_MAP_NOT_SUPPORTED);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MeConfigProfile {
    Conservative,
    D80Ht40SgiVhtHe,
}

impl MeConfigProfile {
    const fn ht_capabilities(self) -> HtCapabilities {
        match self {
            Self::Conservative => HtCapabilities::CONSERVATIVE,
            Self::D80Ht40SgiVhtHe => HtCapabilities::D80_HT40_SGI,
        }
    }

    const fn vht_capabilities(self) -> Option<VhtCapabilities> {
        match self {
            Self::Conservative => None,
            Self::D80Ht40SgiVhtHe => Some(VhtCapabilities::D80_2GHZ),
        }
    }

    const fn he_capabilities(self) -> Option<HeCapabilities> {
        match self {
            Self::Conservative => None,
            Self::D80Ht40SgiVhtHe => Some(HeCapabilities::D80_2GHZ),
        }
    }
}

#[derive(Clone, Copy)]
struct AmpduParameters {
    max_length_factor: u8,
    minimum_spacing: u8,
}

impl AmpduParameters {
    const VENDOR_DEFAULT: Self = Self {
        max_length_factor: 3,
        minimum_spacing: 7,
    };

    const fn encode(self) -> u8 {
        self.max_length_factor | (self.minimum_spacing << 2)
    }
}

#[derive(Clone, Copy)]
struct HtCapabilities {
    capability_info: u16,
    ampdu: AmpduParameters,
    rx_mask: [u8; HT_MCS_RX_MASK_LEN],
    rx_highest: u16,
    tx_params: u8,
}

impl HtCapabilities {
    const CONSERVATIVE: Self = Self {
        capability_info: HT_CAP_LDPC,
        ampdu: AmpduParameters::VENDOR_DEFAULT,
        rx_mask: [0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        rx_highest: 65,
        tx_params: HT_MCS_TX_DEFINED,
    };

    const D80_HT40_SGI: Self = Self {
        // A-MSDU reception up to 7935 octets: the vendor declares it for the
        // chips whose firmware reports A-MSDU support, which the VHT field of
        // this same profile confirms for D80.
        capability_info: HT_CAP_LDPC
            | HT_CAP_WIDTH_20_40
            | HT_CAP_SGI_20
            | HT_CAP_SGI_40
            | HT_CAP_MAX_AMSDU,
        ampdu: AmpduParameters::VENDOR_DEFAULT,
        rx_mask: [0xff, 0, 0, 0, 1, 0, 0, 0, 0, 0],
        rx_highest: 150,
        tx_params: HT_MCS_TX_DEFINED,
    };

    fn encode_into(self, payload: &mut [u8; ME_CONFIG_PAYLOAD_LEN]) {
        payload[HT_CAPABILITY_INFO_OFFSET..HT_CAPABILITY_INFO_OFFSET + 2]
            .copy_from_slice(&self.capability_info.to_le_bytes());
        payload[HT_AMPDU_PARAM_OFFSET] = self.ampdu.encode();
        payload[HT_MCS_OFFSET..HT_MCS_OFFSET + self.rx_mask.len()].copy_from_slice(&self.rx_mask);
        payload[HT_MCS_RX_HIGHEST_OFFSET..HT_MCS_RX_HIGHEST_OFFSET + 2]
            .copy_from_slice(&self.rx_highest.to_le_bytes());
        payload[HT_MCS_TX_PARAMS_OFFSET] = self.tx_params;
        payload[HT_MCS_RESERVED_OFFSET..HT_MCS_OFFSET + HT_MCS_LEN].fill(0);
    }
}

#[derive(Clone, Copy)]
struct VhtCapabilities {
    capability_info: u32,
    rx_mcs_map: u16,
    rx_highest: u16,
    tx_mcs_map: u16,
    tx_highest: u16,
}

impl VhtCapabilities {
    /// VHT capabilities of the 2.4 GHz band on D80 with a single stream: MCS
    /// 0-9 for one stream, 40 MHz wide at most, and no 80 MHz signalling.
    const D80_2GHZ: Self = Self {
        capability_info: VHT_CAP_MAX_MPDU_LENGTH_7991
            | VHT_CAP_RXLDPC
            | VHT_CAP_RXSTBC_1
            | VHT_CAP_SU_BEAMFORMEE_CAPABLE
            | VHT_CAP_BEAMFORMEE_STS_3
            | VHT_CAP_MU_BEAMFORMER_CAPABLE
            | VHT_CAP_MU_BEAMFORMEE_CAPABLE
            | VHT_CAP_MAX_A_MPDU_LENGTH_EXPONENT_7,
        rx_mcs_map: single_stream_mcs_map(VHT_MCS_SUPPORT_0_9),
        rx_highest: VHT_HIGHEST_RATE_1SS_MCS_0_9,
        tx_mcs_map: single_stream_mcs_map(VHT_MCS_SUPPORT_0_9),
        tx_highest: VHT_HIGHEST_RATE_1SS_MCS_0_9,
    };

    fn encode_into(self, payload: &mut [u8; ME_CONFIG_PAYLOAD_LEN]) {
        payload[VHT_CAPABILITY_INFO_OFFSET..VHT_CAPABILITY_INFO_OFFSET + 4]
            .copy_from_slice(&self.capability_info.to_le_bytes());
        payload[VHT_RX_MCS_MAP_OFFSET..VHT_RX_MCS_MAP_OFFSET + 2]
            .copy_from_slice(&self.rx_mcs_map.to_le_bytes());
        payload[VHT_RX_HIGHEST_OFFSET..VHT_RX_HIGHEST_OFFSET + 2]
            .copy_from_slice(&self.rx_highest.to_le_bytes());
        payload[VHT_TX_MCS_MAP_OFFSET..VHT_TX_MCS_MAP_OFFSET + 2]
            .copy_from_slice(&self.tx_mcs_map.to_le_bytes());
        payload[VHT_TX_HIGHEST_OFFSET..VHT_TX_HIGHEST_OFFSET + 2]
            .copy_from_slice(&self.tx_highest.to_le_bytes());
    }
}

#[derive(Clone, Copy)]
struct HeCapabilities {
    mac_cap_info: [u8; HE_MAC_CAP_INFO_LEN],
    phy_cap_info: [u8; HE_PHY_CAP_INFO_LEN],
    rx_mcs_80: u16,
    tx_mcs_80: u16,
    rx_mcs_160: u16,
    tx_mcs_160: u16,
    rx_mcs_80p80: u16,
    tx_mcs_80p80: u16,
    ppe_thresholds: [u8; HE_PPE_THRESHOLD_LEN],
}

impl HeCapabilities {
    /// HE capabilities of the 2.4 GHz band on D80 with a single stream: MCS
    /// 0-11 for one stream, 40 MHz operation in 2.4 GHz, and the PPE threshold
    /// bytes the vendor packs for 40 MHz and 80 MHz operation.
    const D80_2GHZ: Self = Self {
        mac_cap_info: [0, 0, HE_MAC_CAP2_ALL_ACK, 0, 0, 0],
        phy_cap_info: [
            HE_PHY_CAP0_WIDTH_40MHZ_IN_2G | HE_PHY_CAP0_WIDTH_40MHZ_80MHZ_IN_5G,
            HE_PHY_CAP1_LDPC_CODING_IN_PAYLOAD
                | HE_PHY_CAP1_HE_LTF_AND_GI_FOR_HE_PPDUS_0_8US
                | HE_PHY_CAP1_MIDAMBLE_RX_TX_MAX_NSTS,
            HE_PHY_CAP2_MIDAMBLE_RX_TX_MAX_NSTS
                | HE_PHY_CAP2_NDP_4X_LTF_AND_3_2US
                | HE_PHY_CAP2_STBC_RX_UNDER_80MHZ
                | HE_PHY_CAP2_DOPPLER_RX,
            HE_PHY_CAP3_DCM_MAX_CONST_RX_16_QAM | HE_PHY_CAP3_RX_HE_MU_PPDU_FROM_NON_AP_STA,
            HE_PHY_CAP4_SU_BEAMFORMEE | HE_PHY_CAP4_BEAMFORMEE_MAX_STS_UNDER_80MHZ_4,
            HE_PHY_CAP5_NG16_SU_FEEDBACK | HE_PHY_CAP5_NG16_MU_FEEDBACK,
            HE_PHY_CAP6_CODEBOOK_SIZE_42_SU
                | HE_PHY_CAP6_CODEBOOK_SIZE_75_MU
                | HE_PHY_CAP6_TRIG_SU_BEAMFORMER_FB
                | HE_PHY_CAP6_TRIG_MU_BEAMFORMER_FB
                | HE_PHY_CAP6_PPE_THRESHOLD_PRESENT
                | HE_PHY_CAP6_PARTIAL_BANDWIDTH_DL_MUMIMO,
            0,
            HE_PHY_CAP8_20MHZ_IN_40MHZ_HE_PPDU_IN_2G,
            HE_PHY_CAP9_RX_FULL_BW_SU_USING_MU_WITH_COMP_SIGB
                | HE_PHY_CAP9_RX_FULL_BW_SU_USING_MU_WITH_NON_COMP_SIGB,
            0,
        ],
        rx_mcs_80: single_stream_mcs_map(HE_MCS_SUPPORT_0_11),
        tx_mcs_80: single_stream_mcs_map(HE_MCS_SUPPORT_0_11),
        rx_mcs_160: UNSUPPORTED_MCS_MAP,
        tx_mcs_160: UNSUPPORTED_MCS_MAP,
        rx_mcs_80p80: UNSUPPORTED_MCS_MAP,
        tx_mcs_80p80: UNSUPPORTED_MCS_MAP,
        ppe_thresholds: [
            HE_PPE_THRESHOLD_BASE_0
                | HE_PPE_THRESHOLD_40MHZ_MARKER_0
                | HE_PPE_THRESHOLD_80MHZ_MARKER_0,
            HE_PPE_THRESHOLD_BASE_1,
            HE_PPE_THRESHOLD_BASE_2 | HE_PPE_THRESHOLD_80MHZ_MARKER_2,
            HE_PPE_THRESHOLD_80MHZ_MARKER_3,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
    };

    fn encode_into(self, payload: &mut [u8; ME_CONFIG_PAYLOAD_LEN]) {
        payload[HE_MAC_CAP_INFO_OFFSET..HE_MAC_CAP_INFO_OFFSET + HE_MAC_CAP_INFO_LEN]
            .copy_from_slice(&self.mac_cap_info);
        payload[HE_PHY_CAP_INFO_OFFSET..HE_PHY_CAP_INFO_OFFSET + HE_PHY_CAP_INFO_LEN]
            .copy_from_slice(&self.phy_cap_info);
        payload[HE_MCS_PADDING_OFFSET] = 0;
        for (index, mcs_map) in [
            self.rx_mcs_80,
            self.tx_mcs_80,
            self.rx_mcs_160,
            self.tx_mcs_160,
            self.rx_mcs_80p80,
            self.tx_mcs_80p80,
        ]
        .into_iter()
        .enumerate()
        {
            let offset = HE_MCS_SUPPORT_OFFSET + index * 2;
            payload[offset..offset + 2].copy_from_slice(&mcs_map.to_le_bytes());
        }
        payload[HE_PPE_THRESHOLDS_OFFSET..HE_PPE_THRESHOLDS_OFFSET + HE_PPE_THRESHOLD_LEN]
            .copy_from_slice(&self.ppe_thresholds);
    }
}

pub(crate) fn me_config_payload(profile: MeConfigProfile) -> [u8; ME_CONFIG_PAYLOAD_LEN] {
    let mut payload = [0; ME_CONFIG_PAYLOAD_LEN];
    profile.ht_capabilities().encode_into(&mut payload);

    // These capability structures are naturally aligned in the vendor C ABI:
    // HT occupies 32 bytes, VHT 12 bytes, and HE 56 bytes before tx_lft.  A
    // profile without VHT or HE leaves its region at the payload's zero fill.
    let vht = profile.vht_capabilities();
    let he = profile.he_capabilities();
    if let Some(vht) = vht {
        vht.encode_into(&mut payload);
    }
    if let Some(he) = he {
        he.encode_into(&mut payload);
    }
    payload[ME_CONFIG_TX_LIFETIME_OFFSET..ME_CONFIG_TX_LIFETIME_OFFSET + 2]
        .copy_from_slice(&1000u16.to_le_bytes());
    payload[ME_CONFIG_PHY_BW_OFFSET] = ME_CONFIG_PHY_BW_MAX;
    payload[ME_CONFIG_HT_SUPPORTED_OFFSET] = 1;
    payload[ME_CONFIG_VHT_SUPPORTED_OFFSET] = if vht.is_some() { 1 } else { 0 };
    payload[ME_CONFIG_HE_SUPPORTED_OFFSET] = if he.is_some() { 1 } else { 0 };
    payload[ME_CONFIG_HE_UL_ON_OFFSET] = 0;
    payload[ME_CONFIG_PS_ON_OFFSET] = 1;
    payload[ME_CONFIG_ANT_DIV_ON_OFFSET] = 0;
    payload[ME_CONFIG_DPSM_OFFSET] = 0;
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
    fn ap_start_confirmation_requires_the_requested_vif_and_complete_firmware_layout() {
        assert_eq!(parse_ap_start(&[0, 1, 2, 3], 1), Ok(()));
        for payload in [
            &[0][..],
            &[0, 1, 2, 3, 0],
            &[0, 2, 2, 3],
            &[0, 1, 255, 3],
            &[0, 1, 2, 255],
        ] {
            assert_eq!(parse_ap_start(payload, 1), Err(AicError::MalformedResponse));
        }
        assert_eq!(
            parse_ap_start(&[5, 1, 2, 3], 1),
            Err(AicError::FirmwareRejected {
                message_id: APM_START_CFM,
                status: 5
            })
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
    fn me_config_profiles_encode_the_vendor_ht_and_scalar_fields() {
        let conservative = me_config_payload(MeConfigProfile::Conservative);
        let d80 = me_config_payload(MeConfigProfile::D80Ht40SgiVhtHe);
        let expected_ampdu = [3 | (7 << 2)];
        let mut conservative_reference = [0; 112];
        conservative_reference[0..2].copy_from_slice(&1u16.to_le_bytes());
        conservative_reference[2] = 31;
        conservative_reference[3] = 0xff;
        conservative_reference[13..15].copy_from_slice(&65u16.to_le_bytes());
        conservative_reference[15] = 1;
        conservative_reference[100..102].copy_from_slice(&1000u16.to_le_bytes());
        conservative_reference[102] = 2;
        conservative_reference[103] = 1;
        conservative_reference[107] = 1;
        assert_eq!(conservative, conservative_reference);

        let mut d80_reference = conservative_reference;
        d80_reference[0..2].copy_from_slice(&0x0863u16.to_le_bytes());
        d80_reference[7] = 1; // MCS32 in rx_mask[4].
        d80_reference[13..15].copy_from_slice(&150u16.to_le_bytes());
        d80_reference[102] = 2; // PHY_CHNL_BW_80.
        d80_reference[104] = 1; // VHT support.
        d80_reference[105] = 1; // HE support.
        assert_eq!(
            d80[..ME_CONFIG_VHT_OFFSET],
            d80_reference[..ME_CONFIG_VHT_OFFSET]
        );
        assert_eq!(
            &d80[ME_CONFIG_TX_LIFETIME_OFFSET..],
            &d80_reference[ME_CONFIG_TX_LIFETIME_OFFSET..]
        );

        assert_eq!(conservative.len(), ME_CONFIG_PAYLOAD_LEN);
        assert_eq!(d80.len(), ME_CONFIG_PAYLOAD_LEN);
        assert_eq!(
            &conservative[HT_CAPABILITY_INFO_OFFSET..HT_CAPABILITY_INFO_OFFSET + 2],
            &HT_CAP_LDPC.to_le_bytes()
        );
        assert_eq!(
            &d80[HT_CAPABILITY_INFO_OFFSET..HT_CAPABILITY_INFO_OFFSET + 2],
            &(HT_CAP_LDPC | HT_CAP_WIDTH_20_40 | HT_CAP_SGI_20 | HT_CAP_SGI_40 | HT_CAP_MAX_AMSDU)
                .to_le_bytes()
        );
        assert_eq!(
            d80[HT_CAPABILITY_INFO_OFFSET + 1] & (HT_CAP_MAX_AMSDU >> 8) as u8,
            (HT_CAP_MAX_AMSDU >> 8) as u8,
            "the D80 profile declares A-MSDU reception"
        );
        assert_eq!(
            conservative[HT_CAPABILITY_INFO_OFFSET + 1] & (HT_CAP_MAX_AMSDU >> 8) as u8,
            0,
            "the conservative profile keeps its validated bytes"
        );
        assert_eq!(
            conservative[HT_AMPDU_PARAM_OFFSET..HT_AMPDU_PARAM_OFFSET + 1],
            expected_ampdu
        );
        assert_eq!(
            d80[HT_AMPDU_PARAM_OFFSET..HT_AMPDU_PARAM_OFFSET + 1],
            expected_ampdu
        );
        assert_eq!(
            &conservative[HT_MCS_OFFSET..HT_MCS_OFFSET + HT_MCS_RX_MASK_LEN],
            &[0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &d80[HT_MCS_OFFSET..HT_MCS_OFFSET + HT_MCS_RX_MASK_LEN],
            &[0xff, 0, 0, 0, 1, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &conservative[HT_MCS_RX_HIGHEST_OFFSET..HT_MCS_RX_HIGHEST_OFFSET + 2],
            &65u16.to_le_bytes()
        );
        assert_eq!(
            &d80[HT_MCS_RX_HIGHEST_OFFSET..HT_MCS_RX_HIGHEST_OFFSET + 2],
            &150u16.to_le_bytes()
        );
        assert_eq!(conservative[HT_MCS_TX_PARAMS_OFFSET], HT_MCS_TX_DEFINED);
        assert_eq!(d80[HT_MCS_TX_PARAMS_OFFSET], HT_MCS_TX_DEFINED);
        assert_eq!(
            &d80[HT_MCS_RESERVED_OFFSET..ME_CONFIG_VHT_OFFSET],
            &[0; 32 - HT_MCS_RESERVED_OFFSET]
        );
        assert_eq!(
            &conservative[ME_CONFIG_TX_LIFETIME_OFFSET..ME_CONFIG_TX_LIFETIME_OFFSET + 2],
            &1000u16.to_le_bytes()
        );
        assert_eq!(
            &d80[ME_CONFIG_TX_LIFETIME_OFFSET..ME_CONFIG_TX_LIFETIME_OFFSET + 2],
            &1000u16.to_le_bytes()
        );
        assert_eq!(conservative[ME_CONFIG_PHY_BW_OFFSET], 2);
        assert_eq!(d80[ME_CONFIG_PHY_BW_OFFSET], 2);
        assert_eq!(d80[ME_CONFIG_HT_SUPPORTED_OFFSET], 1);
        assert_eq!(d80[ME_CONFIG_VHT_SUPPORTED_OFFSET], 1);
        assert_eq!(d80[ME_CONFIG_HE_SUPPORTED_OFFSET], 1);
        assert_eq!(d80[ME_CONFIG_HE_UL_ON_OFFSET], 0);
        assert_eq!(d80[ME_CONFIG_PS_ON_OFFSET], 1);
        assert_eq!(d80[ME_CONFIG_ANT_DIV_ON_OFFSET], 0);
        assert_eq!(d80[ME_CONFIG_DPSM_OFFSET], 0);
        assert_eq!(&d80[110..ME_CONFIG_PAYLOAD_LEN], &[0; 2]);
        assert_eq!(
            &conservative[ME_CONFIG_VHT_OFFSET..ME_CONFIG_HE_OFFSET],
            &[0; 12]
        );
        assert_eq!(
            &conservative[ME_CONFIG_HE_OFFSET..ME_CONFIG_TX_LIFETIME_OFFSET],
            &[0; 56]
        );
        assert_eq!(conservative[ME_CONFIG_VHT_SUPPORTED_OFFSET], 0);
        assert_eq!(conservative[ME_CONFIG_HE_SUPPORTED_OFFSET], 0);
    }

    #[test]
    fn d80_profile_declares_the_vendor_vht_and_he_capabilities() {
        let payload = me_config_payload(MeConfigProfile::D80Ht40SgiVhtHe);

        assert_eq!(
            &payload[VHT_CAPABILITY_INFO_OFFSET..VHT_CAPABILITY_INFO_OFFSET + 4],
            &0x0398_7111u32.to_le_bytes()
        );
        assert_eq!(
            &payload[VHT_RX_MCS_MAP_OFFSET..VHT_RX_MCS_MAP_OFFSET + 2],
            &0xfffeu16.to_le_bytes()
        );
        assert_eq!(
            &payload[VHT_RX_HIGHEST_OFFSET..VHT_RX_HIGHEST_OFFSET + 2],
            &390u16.to_le_bytes()
        );
        assert_eq!(
            &payload[VHT_TX_MCS_MAP_OFFSET..VHT_TX_MCS_MAP_OFFSET + 2],
            &0xfffeu16.to_le_bytes()
        );
        assert_eq!(
            &payload[VHT_TX_HIGHEST_OFFSET..VHT_TX_HIGHEST_OFFSET + 2],
            &390u16.to_le_bytes()
        );

        assert_eq!(
            &payload[HE_MAC_CAP_INFO_OFFSET..HE_PHY_CAP_INFO_OFFSET],
            &[0x00, 0x00, 0x02, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            &payload[HE_PHY_CAP_INFO_OFFSET..HE_MCS_PADDING_OFFSET],
            &[
                0x06, 0xe0, 0x2b, 0x58, 0x0d, 0xc0, 0xcf, 0x00, 0x02, 0x30, 0x00
            ]
        );
        assert_eq!(payload[HE_MCS_PADDING_OFFSET], 0);
        assert_eq!(
            &payload[HE_MCS_SUPPORT_OFFSET..HE_MCS_SUPPORT_OFFSET + 12],
            &[
                0xfe, 0xff, 0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff
            ]
        );
        assert_eq!(
            &payload[HE_PPE_THRESHOLDS_OFFSET..HE_PPE_THRESHOLDS_OFFSET + 25],
            &[
                0x38, 0x1c, 0xc7, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0
            ]
        );
        // The trailing byte of the 56-byte HE structure is ABI padding.
        assert_eq!(payload[ME_CONFIG_TX_LIFETIME_OFFSET - 1], 0);
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
        // The ids are the vendor firmware's LMAC message indices: renumbering
        // one silently turns an unsolicited message into a confirmation the
        // mailbox is not waiting for, which fails the device.
        assert_eq!(ME_TX_CREDITS_UPDATE_IND, 0x140b);
        assert_eq!(ME_TRAFFIC_IND_CFM, 0x140d);
        assert!(is_indication_message(ME_TX_CREDITS_UPDATE_IND));
        assert!(is_indication_message(ME_TRAFFIC_IND_CFM));
        assert!(!is_indication_message(ME_SET_CONTROL_PORT_CFM));
    }
}
