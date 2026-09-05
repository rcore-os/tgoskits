use alloc::{boxed::Box, collections::VecDeque, vec, vec::Vec};

use super::{AicError, ControlRequest, Entropy, Pmk};
use crate::{
    lmac::{
        ME_SET_CONTROL_PORT_CFM, ME_SET_CONTROL_PORT_REQ, MM_KEY_ADD_CFM, MM_KEY_ADD_REQ,
        SM_CONNECT_CFM, SM_CONNECT_REQ, SM_DISCONNECT_CFM, SM_DISCONNECT_REQ, TASK_ME, TASK_MM,
        TASK_SM, connect_payload, control_port_payload, disconnect_payload, key_add_payload,
    },
    wpa2::{HandshakeAction, Wpa2Handshake, WpaError},
};

const MAX_SSID_LENGTH: usize = 32;

pub(super) struct ControlCommand {
    pub message_id: u16,
    pub destination: u16,
    pub expected_message_id: u16,
    pub payload: Vec<u8>,
}

pub(super) struct ControlState {
    pub commands: VecDeque<ControlCommand>,
    pub operation: ControlOperation,
}

pub(super) enum ControlOperation {
    Commands,
    Connect(Box<PendingConnect>),
    Disconnect,
}

pub(super) struct PendingConnect {
    pub pmk: Option<Pmk>,
    pub entropy: Option<Entropy>,
    pub phase: ConnectPhase,
    handshake: Option<Wpa2Handshake>,
    pending_m4: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConnectPhase {
    /// Clear a possible association left in firmware across a warm host reset.
    /// Linux keeps this state in cfg80211; the bare-metal owner must establish
    /// the same invariant explicitly before issuing a new connect request.
    Resetting,
    AwaitConfirmation,
    AwaitIndication,
    AwaitHandshake,
    InstallingKeys(u8),
    AwaitM4Transmit,
    AwaitControlPort,
}

pub(super) enum ControlEffect {
    None,
    TransmitEapol(Vec<u8>),
}

impl ControlState {
    pub(super) fn accept_connect_indication(
        &mut self,
        station_index: u8,
        bssid: [u8; 6],
        local_mac: [u8; 6],
    ) -> Result<(), AicError> {
        let ControlOperation::Connect(connect) = &mut self.operation else {
            return Err(AicError::CompletionMismatch);
        };
        if connect.phase != ConnectPhase::AwaitIndication {
            return Err(AicError::CompletionMismatch);
        }
        if connect.pmk.is_none() {
            self.commands.push_back(ControlCommand {
                message_id: ME_SET_CONTROL_PORT_REQ,
                destination: TASK_ME,
                expected_message_id: ME_SET_CONTROL_PORT_CFM,
                payload: control_port_payload(station_index, true).to_vec(),
            });
            connect.phase = ConnectPhase::AwaitControlPort;
            Ok(())
        } else {
            let entropy = connect.entropy.take().ok_or(AicError::EntropyUnavailable)?;
            let pmk = connect.pmk.take().ok_or(AicError::WpaProtocol)?;
            connect.handshake = Some(Wpa2Handshake::new(
                *pmk.bytes(),
                bssid,
                local_mac,
                *entropy.bytes(),
            ));
            connect.phase = ConnectPhase::AwaitHandshake;
            Ok(())
        }
    }

    pub(super) fn process_eapol(
        &mut self,
        interface_index: u8,
        station_index: u8,
        eapol: &[u8],
    ) -> Result<ControlEffect, AicError> {
        let ControlOperation::Connect(connect) = &mut self.operation else {
            return Err(AicError::CompletionMismatch);
        };
        if connect.phase != ConnectPhase::AwaitHandshake {
            return Err(AicError::WpaProtocol);
        }
        log::info!("[wifi] WPA2 EAPOL frame consumed by handshake state machine");
        let action = connect
            .handshake
            .as_mut()
            .ok_or(AicError::WpaProtocol)?
            .process(eapol)
            .map_err(map_wpa_error)?;
        match action {
            HandshakeAction::SendM2(frame) => Ok(ControlEffect::TransmitEapol(frame)),
            HandshakeAction::InstallKeys(keys) => {
                self.commands.push_back(ControlCommand {
                    message_id: MM_KEY_ADD_REQ,
                    destination: TASK_MM,
                    expected_message_id: MM_KEY_ADD_CFM,
                    payload: key_add_payload(
                        interface_index,
                        station_index,
                        true,
                        0,
                        &keys.temporal_key,
                    )?
                    .to_vec(),
                });
                self.commands.push_back(ControlCommand {
                    message_id: MM_KEY_ADD_REQ,
                    destination: TASK_MM,
                    expected_message_id: MM_KEY_ADD_CFM,
                    payload: key_add_payload(
                        interface_index,
                        u8::MAX,
                        false,
                        keys.group_key_index,
                        &keys.group_key,
                    )?
                    .to_vec(),
                });
                connect.pending_m4 = Some(keys.m4.clone());
                connect.phase = ConnectPhase::InstallingKeys(2);
                Ok(ControlEffect::None)
            }
        }
    }

    pub(super) fn accept_key_confirmation(&mut self) -> Result<Option<Vec<u8>>, AicError> {
        let ControlOperation::Connect(connect) = &mut self.operation else {
            return Err(AicError::CompletionMismatch);
        };
        let ConnectPhase::InstallingKeys(remaining) = connect.phase else {
            return Err(AicError::CompletionMismatch);
        };
        if remaining > 1 {
            connect.phase = ConnectPhase::InstallingKeys(remaining - 1);
            Ok(None)
        } else {
            connect.phase = ConnectPhase::AwaitM4Transmit;
            connect
                .pending_m4
                .take()
                .map(Some)
                .ok_or(AicError::WpaProtocol)
        }
    }

    pub(super) fn accept_m4_transmit(&mut self, station_index: u8) -> Result<(), AicError> {
        let ControlOperation::Connect(connect) = &mut self.operation else {
            return Err(AicError::CompletionMismatch);
        };
        if connect.phase != ConnectPhase::AwaitM4Transmit {
            return Err(AicError::CompletionMismatch);
        }
        self.commands.push_back(ControlCommand {
            message_id: ME_SET_CONTROL_PORT_REQ,
            destination: TASK_ME,
            expected_message_id: ME_SET_CONTROL_PORT_CFM,
            payload: control_port_payload(station_index, true).to_vec(),
        });
        connect.phase = ConnectPhase::AwaitControlPort;
        Ok(())
    }
}

fn map_wpa_error(error: WpaError) -> AicError {
    match error {
        WpaError::Mic => AicError::WpaMic,
        WpaError::ReplayCounter => AicError::WpaReplay,
        WpaError::Rsn => AicError::WpaRsn,
        WpaError::InvalidKeyData | WpaError::KeyUnwrap | WpaError::GtkMissing => {
            AicError::WpaKeyData
        }
        WpaError::FrameTooShort
        | WpaError::InvalidLength
        | WpaError::InvalidDescriptor
        | WpaError::UnexpectedMessage
        | WpaError::InvalidState => AicError::WpaProtocol,
    }
}

pub(super) fn build(
    request: ControlRequest,
    mac: [u8; 6],
    interface_index: Option<u8>,
) -> Result<ControlState, AicError> {
    let mut commands = VecDeque::new();
    let operation = match request {
        ControlRequest::Scan { ssid } => {
            if ssid
                .as_ref()
                .is_some_and(|ssid| ssid.len() > MAX_SSID_LENGTH)
            {
                return Err(AicError::InvalidControlRequest);
            }
            commands.push_back(scan_command(ssid.as_deref()));
            ControlOperation::Commands
        }
        ControlRequest::Connect { ssid, pmk, entropy } => {
            if ssid.is_empty() || ssid.len() > MAX_SSID_LENGTH {
                return Err(AicError::InvalidControlRequest);
            }
            let entropy = if pmk.is_none() {
                None
            } else {
                Some(entropy.ok_or(AicError::EntropyUnavailable)?)
            };
            let interface_index = interface_index.ok_or(AicError::InvalidControlRequest)?;
            commands.push_back(ControlCommand {
                message_id: SM_DISCONNECT_REQ,
                destination: TASK_SM,
                expected_message_id: SM_DISCONNECT_CFM,
                payload: disconnect_payload(interface_index).to_vec(),
            });
            commands.push_back(ControlCommand {
                message_id: SM_CONNECT_REQ,
                destination: TASK_SM,
                expected_message_id: SM_CONNECT_CFM,
                payload: connect_payload(&ssid, pmk.is_some(), interface_index),
            });
            ControlOperation::Connect(Box::new(PendingConnect {
                pmk,
                entropy,
                phase: ConnectPhase::Resetting,
                handshake: None,
                pending_m4: None,
            }))
        }
        ControlRequest::Disconnect => {
            let interface_index = interface_index.ok_or(AicError::InvalidControlRequest)?;
            commands.push_back(ControlCommand {
                message_id: SM_DISCONNECT_REQ,
                destination: TASK_SM,
                expected_message_id: SM_DISCONNECT_CFM,
                payload: disconnect_payload(interface_index).to_vec(),
            });
            ControlOperation::Disconnect
        }
        ControlRequest::StartOpenAccessPoint { ssid, channel } => {
            if ssid.is_empty() || ssid.len() > MAX_SSID_LENGTH || !(1..=14).contains(&channel) {
                return Err(AicError::InvalidControlRequest);
            }
            commands.extend(open_access_point_commands(&ssid, channel, mac));
            ControlOperation::Commands
        }
        ControlRequest::Cancel | ControlRequest::Shutdown => {
            return Err(AicError::InvalidControlRequest);
        }
    };
    Ok(ControlState {
        commands,
        operation,
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
        expected_message_id: 0x1001,
        payload,
    }
}

/// Offset of the `vif_idx` field inside the natural-aligned vendor
/// `struct apm_start_req` payload built by [`open_access_point_commands`].
///
/// Vendor layout (lmac_msg.h, natural alignment): `mac_rateset` (length + 12
/// rates = 13 bytes), one pad byte, `mac_chan_def` { u16 freq; u8 band;
/// u8 flags; s8 tx_power } at 14..19, `center_freq1/2` u32 at 20..28,
/// `ch_width` at 28, pad, `bcn_addr` u32 at 32, `bcn_len`/`tim_oft`/
/// `bcn_int` u16 at 36..42, pad, `flags` u32 at 44, `ctrl_port_ethertype`
/// u16 at 48, `tim_len` at 50, `vif_idx` at 51 (struct size 52).
pub(super) const APM_START_VIF_INDEX: usize = 51;

fn open_access_point_commands(ssid: &[u8], channel: u8, mac: [u8; 6]) -> VecDeque<ControlCommand> {
    use crate::lmac::{
        APM_SET_BEACON_IE_CFM, APM_SET_BEACON_IE_REQ, APM_START_CFM, APM_START_REQ, MM_ADD_IF_CFM,
        MM_ADD_IF_REQ, MM_SET_FILTER_CFM, MM_SET_FILTER_REQ, MM_START_CFM, MM_START_REQ, TASK_MM,
    };
    let mut commands = VecDeque::new();
    let mut add_interface = vec![0; 10];
    add_interface[0] = 2;
    add_interface[2..8].copy_from_slice(&mac);
    commands.push_back(ControlCommand {
        message_id: MM_ADD_IF_REQ,
        destination: TASK_MM,
        expected_message_id: MM_ADD_IF_CFM,
        payload: add_interface,
    });
    commands.push_back(ControlCommand {
        message_id: MM_START_REQ,
        destination: TASK_MM,
        expected_message_id: MM_START_CFM,
        payload: crate::lmac::start_payload().to_vec(),
    });
    commands.push_back(ControlCommand {
        message_id: MM_SET_FILTER_REQ,
        destination: TASK_MM,
        expected_message_id: MM_SET_FILTER_CFM,
        payload: 0x1502_868cu32.to_le_bytes().to_vec(),
    });
    let (beacon, tim_offset) = open_beacon(ssid, channel, mac);
    // Vendor `struct apm_set_bcn_ie_req` layout (natural alignment): vif_idx
    // at 0, one pad byte, bcn_ie_len u16 at 2, bcn_ie at 4. The vif index
    // comes from the ADD_IF confirmation and is pinned in by the mailbox
    // completion path.
    let mut beacon_request = vec![0; 516];
    beacon_request[2..4].copy_from_slice(&(beacon.len() as u16).to_le_bytes());
    beacon_request[4..4 + beacon.len()].copy_from_slice(&beacon);
    commands.push_back(ControlCommand {
        message_id: APM_SET_BEACON_IE_REQ,
        destination: 7,
        expected_message_id: APM_SET_BEACON_IE_CFM,
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
        message_id: APM_START_REQ,
        destination: 7,
        expected_message_id: APM_START_CFM,
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
                    pmk: Some(Pmk::new([3; 32])),
                    entropy: None,
                },
                [0; 6],
                Some(0),
            )
            .err(),
            Some(AicError::EntropyUnavailable)
        );
        assert!(
            build(
                ControlRequest::Connect {
                    ssid: b"network".to_vec(),
                    pmk: Some(Pmk::new([3; 32])),
                    entropy: Some(Entropy::new([7; 32])),
                },
                [0; 6],
                Some(0)
            )
            .is_ok()
        );
    }

    #[test]
    fn connect_request_carries_the_firmware_interface_index() {
        let state = build(
            ControlRequest::Connect {
                ssid: b"network".to_vec(),
                pmk: Some(Pmk::new([3; 32])),
                entropy: Some(Entropy::new([7; 32])),
            },
            [2, 0, 0, 0, 0, 1],
            Some(6),
        )
        .unwrap();

        let connect = state
            .commands
            .iter()
            .find(|command| command.message_id == SM_CONNECT_REQ)
            .expect("connect command follows the stale-association reset");
        assert_eq!(connect.payload[61], 6);
        assert_eq!(
            state.commands.front().unwrap().message_id,
            SM_DISCONNECT_REQ
        );
    }

    #[test]
    fn disconnect_waits_for_confirmation_not_the_async_indication() {
        let state = build(ControlRequest::Disconnect, [2, 0, 0, 0, 0, 1], Some(6)).unwrap();
        let command = state.commands.front().unwrap();

        assert_eq!(command.message_id, SM_DISCONNECT_REQ);
        assert_eq!(command.expected_message_id, SM_DISCONNECT_CFM);
        assert_eq!(command.payload, [3, 0, 6, 0]);
    }
}
