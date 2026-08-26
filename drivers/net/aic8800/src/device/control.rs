use alloc::{collections::VecDeque, vec, vec::Vec};

use super::{AicError, ControlRequest, Entropy};

const MAX_SSID_LENGTH: usize = 32;
const MAX_PASSWORD_LENGTH: usize = 63;

pub(super) struct ControlCommand {
    pub message_id: u16,
    pub destination: u16,
    pub expected_message_id: u16,
    pub payload: Vec<u8>,
}

pub(super) struct ControlState {
    pub commands: VecDeque<ControlCommand>,
    _wpa_nonce: Option<Entropy>,
}

pub(super) fn build(request: ControlRequest, mac: [u8; 6]) -> Result<ControlState, AicError> {
    let mut commands = VecDeque::new();
    let mut nonce = None;
    match request {
        ControlRequest::Scan { ssid } => {
            if ssid
                .as_ref()
                .is_some_and(|ssid| ssid.len() > MAX_SSID_LENGTH)
            {
                return Err(AicError::InvalidControlRequest);
            }
            commands.push_back(scan_command(ssid.as_deref()));
        }
        ControlRequest::Connect {
            ssid,
            password,
            entropy,
        } => {
            if ssid.is_empty()
                || ssid.len() > MAX_SSID_LENGTH
                || password.len() > MAX_PASSWORD_LENGTH
            {
                return Err(AicError::InvalidControlRequest);
            }
            if !password.is_empty() {
                nonce = Some(entropy.ok_or(AicError::EntropyUnavailable)?);
            }
            commands.push_back(connect_command(&ssid, !password.is_empty()));
        }
        ControlRequest::Disconnect => commands.push_back(ControlCommand {
            message_id: 0x1803,
            destination: 6,
            expected_message_id: 0x1804,
            payload: vec![3, 0, 0],
        }),
        ControlRequest::StartOpenAccessPoint { ssid, channel } => {
            if ssid.is_empty() || ssid.len() > MAX_SSID_LENGTH || !(1..=14).contains(&channel) {
                return Err(AicError::InvalidControlRequest);
            }
            commands.extend(open_access_point_commands(&ssid, channel, mac));
        }
        ControlRequest::Cancel | ControlRequest::Shutdown => {
            return Err(AicError::InvalidControlRequest);
        }
    }
    Ok(ControlState {
        commands,
        _wpa_nonce: nonce,
    })
}

fn scan_command(ssid: Option<&[u8]>) -> ControlCommand {
    const CHANNELS: [u16; 14] = [
        2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462, 2467, 2472, 2484,
    ];
    const CHANNEL_SIZE: usize = 8;
    const CHANNEL_MAX: usize = 42;
    const SSID_SIZE: usize = 33;
    const SSID_MAX: usize = 2;
    let mut payload = vec![0; 376];
    for (index, frequency) in CHANNELS.into_iter().enumerate() {
        let offset = index * CHANNEL_SIZE;
        payload[offset..offset + 2].copy_from_slice(&frequency.to_le_bytes());
        payload[offset + 4] = 30;
    }
    let ssid_offset = CHANNEL_MAX * CHANNEL_SIZE;
    let ssid_count = if let Some(ssid) = ssid {
        payload[ssid_offset] = ssid.len() as u8;
        payload[ssid_offset + 1..ssid_offset + 1 + ssid.len()].copy_from_slice(ssid);
        1
    } else {
        0
    };
    let bssid_offset = ssid_offset + SSID_MAX * SSID_SIZE + 1;
    payload[bssid_offset..bssid_offset + 6].fill(0xff);
    let tail = bssid_offset + 8;
    payload[tail + 6] = 0;
    payload[tail + 7] = CHANNELS.len() as u8;
    payload[tail + 8] = ssid_count;
    ControlCommand {
        message_id: 0x1000,
        destination: 4,
        expected_message_id: 0x1009,
        payload,
    }
}

fn connect_command(ssid: &[u8], secured: bool) -> ControlCommand {
    let mut payload = vec![0; 320];
    payload[0] = ssid.len() as u8;
    payload[1..1 + ssid.len()].copy_from_slice(ssid);
    payload[34..40].fill(0xff);
    payload[40..42].copy_from_slice(&0xffffu16.to_le_bytes());
    if secured {
        payload[48..52].copy_from_slice(&0x0000_000du32.to_le_bytes());
    }
    payload[52..54].copy_from_slice(&0x888eu16.to_be_bytes());
    payload[56..58].copy_from_slice(&1u16.to_le_bytes());
    payload[61] = 0;
    ControlCommand {
        message_id: 0x1800,
        destination: 6,
        expected_message_id: 0x1801,
        payload,
    }
}

fn open_access_point_commands(ssid: &[u8], channel: u8, mac: [u8; 6]) -> VecDeque<ControlCommand> {
    let mut commands = VecDeque::new();
    let mut add_interface = vec![0; 10];
    add_interface[0] = 2;
    add_interface[2..8].copy_from_slice(&mac);
    commands.push_back(ControlCommand {
        message_id: 0x0006,
        destination: 0,
        expected_message_id: 0x0007,
        payload: add_interface,
    });
    commands.push_back(ControlCommand {
        message_id: 0x0002,
        destination: 0,
        expected_message_id: 0x0003,
        payload: vec![0; 70],
    });
    commands.push_back(ControlCommand {
        message_id: 0x000e,
        destination: 0,
        expected_message_id: 0x000f,
        payload: 0x1502_868cu32.to_le_bytes().to_vec(),
    });
    let (beacon, tim_offset) = open_beacon(ssid, channel, mac);
    let mut beacon_request = vec![0; 516];
    beacon_request[2..4].copy_from_slice(&(beacon.len() as u16).to_le_bytes());
    beacon_request[4..4 + beacon.len()].copy_from_slice(&beacon);
    commands.push_back(ControlCommand {
        message_id: 0x1c08,
        destination: 7,
        expected_message_id: 0x1c09,
        payload: beacon_request,
    });
    let mut start = vec![0; 52];
    start[0] = 4;
    start[1..5].copy_from_slice(&[0x82, 0x84, 0x8b, 0x96]);
    let frequency = if channel == 14 {
        2484
    } else {
        2407 + u16::from(channel) * 5
    };
    start[14..16].copy_from_slice(&frequency.to_le_bytes());
    start[18] = 20;
    start[20..24].copy_from_slice(&u32::from(frequency).to_le_bytes());
    start[36..38].copy_from_slice(&(beacon.len() as u16).to_le_bytes());
    start[38..40].copy_from_slice(&(tim_offset as u16).to_le_bytes());
    start[40..42].copy_from_slice(&100u16.to_le_bytes());
    start[48..50].copy_from_slice(&0x888eu16.to_be_bytes());
    start[50] = 6;
    commands.push_back(ControlCommand {
        message_id: 0x1c00,
        destination: 7,
        expected_message_id: 0x1c01,
        payload: start,
    });
    commands
}

fn open_beacon(ssid: &[u8], channel: u8, mac: [u8; 6]) -> (Vec<u8>, usize) {
    let mut beacon = Vec::new();
    beacon.extend_from_slice(&[0x80, 0, 0, 0]);
    beacon.extend_from_slice(&[0xff; 6]);
    beacon.extend_from_slice(&mac);
    beacon.extend_from_slice(&mac);
    beacon.extend_from_slice(&[0, 0]);
    beacon.extend_from_slice(&[0; 8]);
    beacon.extend_from_slice(&100u16.to_le_bytes());
    beacon.extend_from_slice(&0x0021u16.to_le_bytes());
    beacon.extend_from_slice(&[0, ssid.len() as u8]);
    beacon.extend_from_slice(ssid);
    beacon.extend_from_slice(&[1, 8, 0x82, 0x84, 0x8b, 0x96, 0x0c, 0x12, 0x18, 0x24]);
    beacon.extend_from_slice(&[3, 1, channel]);
    let tim_offset = beacon.len();
    beacon.extend_from_slice(&[5, 4, 0, 1, 0, 0]);
    (beacon, tim_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_connect_requires_owned_entropy() {
        assert_eq!(
            build(
                ControlRequest::Connect {
                    ssid: b"network".to_vec(),
                    password: b"password".to_vec(),
                    entropy: None,
                },
                [0; 6]
            )
            .err(),
            Some(AicError::EntropyUnavailable)
        );
        assert!(
            build(
                ControlRequest::Connect {
                    ssid: b"network".to_vec(),
                    password: b"password".to_vec(),
                    entropy: Some(Entropy::new([7; 32])),
                },
                [0; 6]
            )
            .is_ok()
        );
    }
}
