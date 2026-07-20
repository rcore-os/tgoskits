//! Pure SoftAP policy and LMAC request construction.

use alloc::{vec, vec::Vec};

pub(crate) const TASK_MM: u16 = 0;
pub(crate) const TASK_ME: u16 = 5;
pub(crate) const TASK_APM: u16 = 7;

pub(crate) const MM_SET_STACK_START_REQ: u16 = 0x007b;
pub(crate) const MM_RESET_REQ: u16 = 0x0000;
pub(crate) const MM_START_REQ: u16 = 0x0002;
pub(crate) const MM_ADD_IF_REQ: u16 = 0x0006;
pub(crate) const MM_SET_FILTER_REQ: u16 = 0x000e;
pub(crate) const MM_SET_RF_CALIB_REQ: u16 = 0x0069;
pub(crate) const MM_GET_MAC_ADDR_REQ: u16 = 0x0073;
pub(crate) const ME_CONFIG_REQ: u16 = 0x1400;
pub(crate) const ME_CHAN_CONFIG_REQ: u16 = 0x1402;
pub(crate) const APM_START_REQ: u16 = 0x1c00;
pub(crate) const APM_SET_BEACON_IE_REQ: u16 = 0x1c08;

const MAX_SSID_LEN: usize = 32;
const AP_MODE_FILTER_DEFAULT: u32 = 0x1502_a79c;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SoftApPolicyError {
    #[error("SoftAP SSID must contain between 1 and 32 bytes")]
    InvalidSsid,
    #[error("SoftAP channel must be in the 2.4 GHz range 1..=14")]
    InvalidChannel,
}

/// Immutable board policy for the default open SoftAP.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SoftApPolicy {
    ssid: [u8; MAX_SSID_LEN],
    ssid_len: u8,
    channel: u8,
}

impl SoftApPolicy {
    pub fn try_new(ssid: &[u8], channel: u8) -> Result<Self, SoftApPolicyError> {
        if ssid.is_empty() || ssid.len() > MAX_SSID_LEN {
            return Err(SoftApPolicyError::InvalidSsid);
        }
        if !(1..=14).contains(&channel) {
            return Err(SoftApPolicyError::InvalidChannel);
        }
        let mut value = Self {
            ssid: [0; MAX_SSID_LEN],
            ssid_len: ssid.len() as u8,
            channel,
        };
        value.ssid[..ssid.len()].copy_from_slice(ssid);
        Ok(value)
    }

    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..usize::from(self.ssid_len)]
    }

    pub const fn channel(&self) -> u8 {
        self.channel
    }
}

pub(crate) struct LmacRequest {
    pub message_id: u16,
    pub destination: u16,
    pub payload: Vec<u8>,
}

pub(crate) fn stack_start_request() -> LmacRequest {
    LmacRequest {
        message_id: MM_SET_STACK_START_REQ,
        destination: TASK_MM,
        payload: vec![1, 0, 0, 0],
    }
}

pub(crate) fn rf_calibration_request() -> LmacRequest {
    let mut payload = vec![0; 22];
    payload[0..4].copy_from_slice(&0xbfu32.to_le_bytes());
    payload[4..8].copy_from_slice(&0x3fu32.to_le_bytes());
    payload[8..12].copy_from_slice(&0x0c34_c008u32.to_le_bytes());
    payload[16..20].copy_from_slice(&0x0026_4203u32.to_le_bytes());
    LmacRequest {
        message_id: MM_SET_RF_CALIB_REQ,
        destination: TASK_MM,
        payload,
    }
}

pub(crate) fn reset_request() -> LmacRequest {
    LmacRequest {
        message_id: MM_RESET_REQ,
        destination: TASK_MM,
        payload: Vec::new(),
    }
}

pub(crate) fn me_config_request() -> LmacRequest {
    let mut payload = vec![0; 102];
    payload[0..2].copy_from_slice(&1u16.to_le_bytes());
    payload[2] = 3 | (7 << 2);
    payload[3] = 0xff;
    payload[94] = 0;
    payload[95] = 1;
    LmacRequest {
        message_id: ME_CONFIG_REQ,
        destination: TASK_ME,
        payload,
    }
}

pub(crate) fn channel_config_request() -> LmacRequest {
    const FREQUENCIES: [u16; 14] = [
        2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462, 2467, 2472, 2484,
    ];
    const CHANNEL_BYTES: usize = 6;
    let mut payload = vec![0; 14 * CHANNEL_BYTES + 28 * CHANNEL_BYTES + 2];
    for (index, frequency) in FREQUENCIES.iter().enumerate() {
        let offset = index * CHANNEL_BYTES;
        payload[offset..offset + 2].copy_from_slice(&frequency.to_le_bytes());
        payload[offset + 4] = 30;
    }
    payload[42 * CHANNEL_BYTES] = FREQUENCIES.len() as u8;
    LmacRequest {
        message_id: ME_CHAN_CONFIG_REQ,
        destination: TASK_ME,
        payload,
    }
}

pub(crate) fn get_mac_request() -> LmacRequest {
    LmacRequest {
        message_id: MM_GET_MAC_ADDR_REQ,
        destination: TASK_MM,
        payload: 1u32.to_le_bytes().to_vec(),
    }
}

pub(crate) fn add_ap_interface_request(mac: [u8; 6]) -> LmacRequest {
    let mut payload = vec![0; 10];
    payload[0] = 2;
    payload[2..8].copy_from_slice(&mac);
    LmacRequest {
        message_id: MM_ADD_IF_REQ,
        destination: TASK_MM,
        payload,
    }
}

pub(crate) fn start_mac_request() -> LmacRequest {
    let mut payload = vec![0; 70];
    payload[64..68].copy_from_slice(&300u32.to_le_bytes());
    payload[68..70].copy_from_slice(&20u16.to_le_bytes());
    LmacRequest {
        message_id: MM_START_REQ,
        destination: TASK_MM,
        payload,
    }
}

pub(crate) fn filter_request() -> LmacRequest {
    LmacRequest {
        message_id: MM_SET_FILTER_REQ,
        destination: TASK_MM,
        payload: AP_MODE_FILTER_DEFAULT.to_le_bytes().to_vec(),
    }
}

fn open_beacon(policy: SoftApPolicy, bssid: [u8; 6]) -> (Vec<u8>, u16, u8) {
    let mut beacon = Vec::with_capacity(96);
    beacon.extend_from_slice(&[0x80, 0, 0, 0]);
    beacon.extend_from_slice(&[0xff; 6]);
    beacon.extend_from_slice(&bssid);
    beacon.extend_from_slice(&bssid);
    beacon.extend_from_slice(&[0, 0]);
    beacon.extend_from_slice(&[0; 8]);
    beacon.extend_from_slice(&100u16.to_le_bytes());
    beacon.extend_from_slice(&0x21u16.to_le_bytes());
    beacon.extend_from_slice(&[0, policy.ssid_len]);
    beacon.extend_from_slice(policy.ssid());
    beacon.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);
    beacon.extend_from_slice(&[3, 1, policy.channel]);
    let tim_offset = beacon.len() as u16;
    beacon.extend_from_slice(&[5, 4, 0, 1, 0, 0]);
    (beacon, tim_offset, 6)
}

pub(crate) fn beacon_request(policy: SoftApPolicy, bssid: [u8; 6], vif: u8) -> LmacRequest {
    let (beacon, ..) = open_beacon(policy, bssid);
    let mut payload = vec![0; 516];
    payload[0] = vif;
    payload[2..4].copy_from_slice(&(beacon.len() as u16).to_le_bytes());
    payload[4..4 + beacon.len()].copy_from_slice(&beacon);
    LmacRequest {
        message_id: APM_SET_BEACON_IE_REQ,
        destination: TASK_APM,
        payload,
    }
}

pub(crate) fn start_ap_request(policy: SoftApPolicy, bssid: [u8; 6], vif: u8) -> LmacRequest {
    let (beacon, tim_offset, tim_len) = open_beacon(policy, bssid);
    let mut payload = vec![0; 52];
    payload[0..5].copy_from_slice(&[4, 0x82, 0x84, 0x8b, 0x96]);
    let frequency = 2407 + 5 * u16::from(policy.channel);
    payload[14..16].copy_from_slice(&frequency.to_le_bytes());
    payload[18] = 20;
    payload[20..24].copy_from_slice(&u32::from(frequency).to_le_bytes());
    payload[36..38].copy_from_slice(&(beacon.len() as u16).to_le_bytes());
    payload[38..40].copy_from_slice(&tim_offset.to_le_bytes());
    payload[40..42].copy_from_slice(&100u16.to_le_bytes());
    payload[48..50].copy_from_slice(&0x888eu16.to_be_bytes());
    payload[50] = tim_len;
    payload[51] = vif;
    LmacRequest {
        message_id: APM_START_REQ,
        destination: TASK_APM,
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_softap_policy_builds_consistent_beacon_metadata() {
        let policy = SoftApPolicy::try_new(b"PicoClaw-Car", 6).unwrap();
        let bssid = [2, 3, 4, 5, 6, 7];
        let beacon = beacon_request(policy, bssid, 1);
        let start = start_ap_request(policy, bssid, 1);
        let beacon_len = u16::from_le_bytes([beacon.payload[2], beacon.payload[3]]);
        assert_eq!(
            beacon_len,
            u16::from_le_bytes([start.payload[36], start.payload[37]])
        );
        assert_eq!(start.payload[51], 1);
    }
}
