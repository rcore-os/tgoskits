//! Pure, allocation-bounded WPA2-PSK/CCMP supplicant state.

use alloc::{vec, vec::Vec};

use aes_kw::{KeyInit as AesKeyInit, KwAes128};
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use sha1::Sha1;
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroize;

const EAPOL_VERSION: u8 = 1;
const EAPOL_TYPE_KEY: u8 = 3;
const KEY_DESCRIPTOR_RSN: u8 = 2;
const KEY_VERSION_HMAC_SHA1_AES: u16 = 0x0002;
const KEY_PAIRWISE: u16 = 0x0008;
const KEY_INSTALL: u16 = 0x0040;
const KEY_ACK: u16 = 0x0080;
const KEY_MIC: u16 = 0x0100;
const KEY_SECURE: u16 = 0x0200;
const KEY_ENCRYPTED: u16 = 0x1000;
const M1_KEY_INFO: u16 = KEY_VERSION_HMAC_SHA1_AES | KEY_PAIRWISE | KEY_ACK;
const M3_KEY_INFO: u16 = KEY_VERSION_HMAC_SHA1_AES
    | KEY_PAIRWISE
    | KEY_INSTALL
    | KEY_ACK
    | KEY_MIC
    | KEY_SECURE
    | KEY_ENCRYPTED;
const EAPOL_HEADER: usize = 4;
const KEY_HEADER: usize = 95;
const MIC_OFFSET: usize = 81;
const MIC_LENGTH: usize = 16;
const PMK_LENGTH: usize = 32;
const PTK_LENGTH: usize = 48;
const SHA1_LENGTH: usize = 20;

type HmacSha1 = Hmac<Sha1>;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub(crate) enum WpaError {
    #[error("truncated EAPOL-Key frame")]
    FrameTooShort,
    #[error("invalid EAPOL-Key frame length")]
    InvalidLength,
    #[error("unsupported EAPOL-Key descriptor")]
    InvalidDescriptor,
    #[error("unexpected WPA2 handshake message")]
    UnexpectedMessage,
    #[error("invalid WPA2 handshake state")]
    InvalidState,
    #[error("EAPOL replay counter did not increase")]
    ReplayCounter,
    #[error("EAPOL MIC verification failed")]
    Mic,
    #[error("invalid encrypted key data")]
    InvalidKeyData,
    #[error("M3 RSN information does not match the requested WPA2 profile")]
    Rsn,
    #[error("AES key unwrap integrity check failed")]
    KeyUnwrap,
    #[error("GTK KDE is missing")]
    GtkMissing,
}

pub(crate) enum HandshakeAction {
    SendM2(Vec<u8>),
    InstallKeys(HandshakeKeys),
}

pub(crate) struct HandshakeKeys {
    pub(crate) m4: Vec<u8>,
    pub(crate) temporal_key: [u8; 16],
    pub(crate) group_key: Vec<u8>,
    pub(crate) group_key_index: u8,
}

impl Drop for HandshakeKeys {
    fn drop(&mut self) {
        self.temporal_key.zeroize();
        self.group_key.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HandshakeState {
    AwaitM1,
    AwaitM3,
    KeysDerived,
}

struct Ptk {
    kck: [u8; 16],
    kek: [u8; 16],
    tk: [u8; 16],
}

impl Drop for Ptk {
    fn drop(&mut self) {
        self.kck.zeroize();
        self.kek.zeroize();
        self.tk.zeroize();
    }
}

struct EapolKey<'a> {
    key_info: u16,
    replay_counter: [u8; 8],
    nonce: [u8; 32],
    mic: [u8; MIC_LENGTH],
    key_data: &'a [u8],
}

pub(crate) struct Wpa2Handshake {
    state: HandshakeState,
    pmk: [u8; PMK_LENGTH],
    ptk: Option<Ptk>,
    anonce: [u8; 32],
    snonce: [u8; 32],
    authenticator: [u8; 6],
    supplicant: [u8; 6],
    replay_counter: [u8; 8],
}

impl Drop for Wpa2Handshake {
    fn drop(&mut self) {
        self.pmk.zeroize();
        self.anonce.zeroize();
        self.snonce.zeroize();
        self.replay_counter.zeroize();
    }
}

impl Wpa2Handshake {
    pub(crate) fn new(
        pmk: [u8; PMK_LENGTH],
        authenticator: [u8; 6],
        supplicant: [u8; 6],
        snonce: [u8; 32],
    ) -> Self {
        Self {
            state: HandshakeState::AwaitM1,
            pmk,
            ptk: None,
            anonce: [0; 32],
            snonce,
            authenticator,
            supplicant,
            replay_counter: [0; 8],
        }
    }

    pub(crate) fn process(&mut self, frame: &[u8]) -> Result<HandshakeAction, WpaError> {
        let key = parse_eapol_key(frame)?;
        match key.key_info {
            M1_KEY_INFO => self.process_m1(key),
            M3_KEY_INFO => self.process_m3(key, frame),
            _ => Err(WpaError::UnexpectedMessage),
        }
    }

    fn process_m1(&mut self, key: EapolKey<'_>) -> Result<HandshakeAction, WpaError> {
        if key.key_info != M1_KEY_INFO {
            return Err(WpaError::UnexpectedMessage);
        }
        match self.state {
            HandshakeState::AwaitM1 => {
                self.anonce = key.nonce;
                self.replay_counter = key.replay_counter;
                self.ptk = Some(derive_ptk(
                    &self.pmk,
                    &self.authenticator,
                    &self.supplicant,
                    &self.anonce,
                    &self.snonce,
                ));
                self.state = HandshakeState::AwaitM3;
            }
            HandshakeState::AwaitM3
                if key.replay_counter == self.replay_counter && key.nonce == self.anonce => {}
            HandshakeState::AwaitM3 => return Err(WpaError::ReplayCounter),
            HandshakeState::KeysDerived => return Err(WpaError::UnexpectedMessage),
        }
        let mut m2 = build_eapol_key(
            KEY_VERSION_HMAC_SHA1_AES | KEY_PAIRWISE | KEY_MIC,
            &self.replay_counter,
            &self.snonce,
            &crate::lmac::RSN_IE_CCMP_PSK,
        );
        let mic = compute_mic(&self.ptk.as_ref().ok_or(WpaError::InvalidState)?.kck, &m2);
        m2[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].copy_from_slice(&mic);
        Ok(HandshakeAction::SendM2(m2))
    }

    fn process_m3(&mut self, key: EapolKey<'_>, frame: &[u8]) -> Result<HandshakeAction, WpaError> {
        if self.state != HandshakeState::AwaitM3 || key.key_info != M3_KEY_INFO {
            return Err(WpaError::InvalidState);
        }
        if key.replay_counter <= self.replay_counter || key.nonce != self.anonce {
            return Err(WpaError::ReplayCounter);
        }
        let ptk = self.ptk.as_ref().ok_or(WpaError::InvalidState)?;
        let mut authenticated = frame.to_vec();
        authenticated[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].fill(0);
        let computed = compute_mic(&ptk.kck, &authenticated);
        if computed.ct_eq(&key.mic).unwrap_u8() != 1 {
            return Err(WpaError::Mic);
        }
        let mut decrypted = aes_key_unwrap(&ptk.kek, key.key_data)?;
        let parsed_key_data = (|| {
            validate_rsn_ie(&decrypted)?;
            parse_gtk_kde(&decrypted)
        })();
        decrypted.zeroize();
        let (mut group_key, group_key_index) = parsed_key_data?;
        if group_key.len() != 16 {
            group_key.zeroize();
            return Err(WpaError::InvalidKeyData);
        }
        self.replay_counter = key.replay_counter;
        let mut m4 = build_eapol_key(
            KEY_VERSION_HMAC_SHA1_AES | KEY_PAIRWISE | KEY_MIC | KEY_SECURE,
            &self.replay_counter,
            &[0; 32],
            &[],
        );
        let mic = compute_mic(&ptk.kck, &m4);
        m4[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].copy_from_slice(&mic);
        self.state = HandshakeState::KeysDerived;
        Ok(HandshakeAction::InstallKeys(HandshakeKeys {
            m4,
            temporal_key: ptk.tk,
            group_key,
            group_key_index,
        }))
    }
}

fn parse_eapol_key(frame: &[u8]) -> Result<EapolKey<'_>, WpaError> {
    if frame.len() < EAPOL_HEADER + KEY_HEADER {
        return Err(WpaError::FrameTooShort);
    }
    let body_length = u16::from_be_bytes([frame[2], frame[3]]) as usize;
    if body_length != frame.len() - EAPOL_HEADER || frame[0] > 2 || frame[1] != EAPOL_TYPE_KEY {
        return Err(WpaError::InvalidLength);
    }
    let offset = EAPOL_HEADER;
    if frame[offset] != KEY_DESCRIPTOR_RSN {
        return Err(WpaError::InvalidDescriptor);
    }
    let key_info = u16::from_be_bytes([frame[offset + 1], frame[offset + 2]]);
    let key_data_length = u16::from_be_bytes([frame[offset + 93], frame[offset + 94]]) as usize;
    if EAPOL_HEADER + KEY_HEADER + key_data_length != frame.len() {
        return Err(WpaError::InvalidLength);
    }
    Ok(EapolKey {
        key_info,
        replay_counter: frame[offset + 5..offset + 13]
            .try_into()
            .map_err(|_| WpaError::FrameTooShort)?,
        nonce: frame[offset + 13..offset + 45]
            .try_into()
            .map_err(|_| WpaError::FrameTooShort)?,
        mic: frame[offset + 77..offset + 93]
            .try_into()
            .map_err(|_| WpaError::FrameTooShort)?,
        key_data: &frame[EAPOL_HEADER + KEY_HEADER..],
    })
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; SHA1_LENGTH] {
    let mut mac =
        <HmacSha1 as HmacKeyInit>::new_from_slice(key).expect("HMAC accepts arbitrary key sizes");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

fn derive_ptk(
    pmk: &[u8; 32],
    authenticator: &[u8; 6],
    supplicant: &[u8; 6],
    anonce: &[u8; 32],
    snonce: &[u8; 32],
) -> Ptk {
    let mut context = [0; 76];
    let (first, second) = if authenticator < supplicant {
        (authenticator, supplicant)
    } else {
        (supplicant, authenticator)
    };
    context[..6].copy_from_slice(first);
    context[6..12].copy_from_slice(second);
    let (first, second) = if anonce < snonce {
        (anonce, snonce)
    } else {
        (snonce, anonce)
    };
    context[12..44].copy_from_slice(first);
    context[44..].copy_from_slice(second);
    let mut bytes = [0; PTK_LENGTH];
    let mut copied = 0;
    for counter in 0..3u8 {
        let mut input = Vec::with_capacity(23 + context.len());
        input.extend_from_slice(b"Pairwise key expansion");
        input.push(0);
        input.extend_from_slice(&context);
        input.push(counter);
        let mut digest = hmac_sha1(pmk, &input);
        let count = SHA1_LENGTH.min(PTK_LENGTH - copied);
        bytes[copied..copied + count].copy_from_slice(&digest[..count]);
        copied += count;
        digest.zeroize();
        input.zeroize();
    }
    let ptk = Ptk {
        kck: bytes[..16].try_into().expect("fixed slice length"),
        kek: bytes[16..32].try_into().expect("fixed slice length"),
        tk: bytes[32..48].try_into().expect("fixed slice length"),
    };
    bytes.zeroize();
    context.zeroize();
    ptk
}

fn compute_mic(kck: &[u8; 16], frame: &[u8]) -> [u8; MIC_LENGTH] {
    let mut digest = hmac_sha1(kck, frame);
    let mic = digest[..MIC_LENGTH].try_into().expect("fixed slice length");
    digest.zeroize();
    mic
}

fn build_eapol_key(
    key_info: u16,
    replay_counter: &[u8; 8],
    nonce: &[u8; 32],
    key_data: &[u8],
) -> Vec<u8> {
    let mut frame = vec![0; EAPOL_HEADER + KEY_HEADER + key_data.len()];
    frame[0] = EAPOL_VERSION;
    frame[1] = EAPOL_TYPE_KEY;
    frame[2..4].copy_from_slice(&((KEY_HEADER + key_data.len()) as u16).to_be_bytes());
    frame[4] = KEY_DESCRIPTOR_RSN;
    frame[5..7].copy_from_slice(&key_info.to_be_bytes());
    frame[9..17].copy_from_slice(replay_counter);
    frame[17..49].copy_from_slice(nonce);
    frame[97..99].copy_from_slice(&(key_data.len() as u16).to_be_bytes());
    frame[99..].copy_from_slice(key_data);
    frame
}

fn aes_key_unwrap(kek: &[u8; 16], wrapped: &[u8]) -> Result<Vec<u8>, WpaError> {
    if wrapped.len() < 16 || !wrapped.len().is_multiple_of(8) {
        return Err(WpaError::KeyUnwrap);
    }
    let cipher = KwAes128::new(&(*kek).into());
    let mut output = vec![0; wrapped.len() - aes_kw::IV_LEN];
    if cipher.unwrap_key(wrapped, &mut output).is_err() {
        output.zeroize();
        return Err(WpaError::KeyUnwrap);
    }
    Ok(output)
}

fn parse_gtk_kde(data: &[u8]) -> Result<(Vec<u8>, u8), WpaError> {
    let mut offset = 0;
    let mut gtk = None;
    while offset < data.len() {
        if data[offset] == 0 {
            offset += 1;
            continue;
        }
        if offset + 2 > data.len() {
            return Err(WpaError::InvalidKeyData);
        }
        let length = data[offset + 1] as usize;
        let end = offset + 2 + length;
        if end > data.len() {
            return Err(WpaError::InvalidKeyData);
        }
        if data[offset] == 0xdd
            && length >= 4
            && data[offset + 2..offset + 6] == [0x00, 0x0f, 0xac, 0x01]
        {
            if length != 22
                || data[offset + 6] & !0x07 != 0
                || data[offset + 7] != 0
                || gtk.is_some()
            {
                return Err(WpaError::InvalidKeyData);
            }
            gtk = Some((data[offset + 8..end].to_vec(), data[offset + 6] & 3));
        }
        offset = end;
    }
    gtk.ok_or(WpaError::GtkMissing)
}

fn validate_rsn_ie(data: &[u8]) -> Result<(), WpaError> {
    // RSN capabilities and optional PMKID fields are selected by the AP, so
    // validation must compare the negotiated algorithms rather than require a
    // byte-for-byte copy of our request IE.
    if data.len() < 2 || data[0] != 0x30 {
        return Err(WpaError::Rsn);
    }
    let element_end = 2usize
        .checked_add(usize::from(data[1]))
        .ok_or(WpaError::Rsn)?;
    if element_end > data.len() || data[1] < 20 {
        return Err(WpaError::Rsn);
    }
    let body = &data[2..element_end];
    if u16::from_le_bytes([body[0], body[1]]) != 1
        || body[2..6] != [0x00, 0x0f, 0xac, 0x04]
        || u16::from_le_bytes([body[6], body[7]]) != 1
        || body[8..12] != [0x00, 0x0f, 0xac, 0x04]
        || u16::from_le_bytes([body[12], body[13]]) != 1
        || body[14..18] != [0x00, 0x0f, 0xac, 0x02]
    {
        return Err(WpaError::Rsn);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode<const N: usize>(hex: &str) -> [u8; N] {
        assert_eq!(hex.len(), N * 2);
        let mut bytes = [0; N];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
        }
        bytes
    }

    fn handshake_after_m1() -> (Wpa2Handshake, [u8; 32], Vec<u8>) {
        let anonce = core::array::from_fn(|index| index as u8);
        let mut handshake = Wpa2Handshake::new(
            decode("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"),
            [0x02, 0, 0, 0, 0, 1],
            [0x02, 0, 0, 0, 0, 2],
            core::array::from_fn(|index| index as u8 + 32),
        );
        let m1 = build_eapol_key(
            KEY_VERSION_HMAC_SHA1_AES | KEY_PAIRWISE | KEY_ACK,
            &1u64.to_be_bytes(),
            &anonce,
            &[],
        );
        let m2 = match handshake.process(&m1).unwrap() {
            HandshakeAction::SendM2(frame) => frame,
            HandshakeAction::InstallKeys(_) => panic!("M1 must produce M2"),
        };
        (handshake, anonce, m2)
    }

    fn m3_with_rsn(handshake: &Wpa2Handshake, anonce: &[u8; 32], rsn: &[u8]) -> Vec<u8> {
        let ptk = handshake.ptk.as_ref().unwrap();
        let mut key_data = rsn.to_vec();
        key_data.extend_from_slice(&[
            0xdd, 22, 0x00, 0x0f, 0xac, 0x01, 0x01, 0x00, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
            0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
        ]);
        while !key_data.len().is_multiple_of(aes_kw::IV_LEN) {
            key_data.push(0);
        }
        let cipher = KwAes128::new(&ptk.kek.into());
        let mut wrapped = vec![0; key_data.len() + aes_kw::IV_LEN];
        cipher.wrap_key(&key_data, &mut wrapped).unwrap();
        key_data.zeroize();
        let mut m3 = build_eapol_key(
            KEY_VERSION_HMAC_SHA1_AES
                | KEY_PAIRWISE
                | KEY_INSTALL
                | KEY_ACK
                | KEY_MIC
                | KEY_SECURE
                | KEY_ENCRYPTED,
            &2u64.to_be_bytes(),
            anonce,
            &wrapped,
        );
        wrapped.zeroize();
        let mic = compute_mic(&ptk.kck, &m3);
        m3[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].copy_from_slice(&mic);
        m3
    }

    #[test]
    fn ptk_prf_matches_fixed_vector() {
        let pmk = decode("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
        let ptk = derive_ptk(
            &pmk,
            &decode("001122334455"),
            &decode("66778899aabb"),
            &core::array::from_fn(|index| index as u8),
            &core::array::from_fn(|index| index as u8 + 32),
        );
        let mut bytes = [0; 48];
        bytes[..16].copy_from_slice(&ptk.kck);
        bytes[16..32].copy_from_slice(&ptk.kek);
        bytes[32..].copy_from_slice(&ptk.tk);
        assert_eq!(
            bytes,
            decode("85c98eca56145629359ac8830bb66a59c5562d473fddcb4eee9ce4de54e1cb1a12cdd4448325c84079abcd76b1b89f8f")
        );
    }

    #[test]
    fn aes_unwrap_matches_rfc3394_vector() {
        assert_eq!(
            aes_key_unwrap(
                &decode("000102030405060708090a0b0c0d0e0f"),
                &decode::<24>("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5")
            ),
            Ok(decode::<16>("00112233445566778899aabbccddeeff").to_vec())
        );
    }

    #[test]
    fn parser_rejects_declared_body_truncation() {
        let mut frame = build_eapol_key(KEY_ACK, &[0; 8], &[0; 32], &[]);
        frame[3] = frame[3].wrapping_add(1);
        assert_eq!(parse_eapol_key(&frame).err(), Some(WpaError::InvalidLength));
    }

    #[test]
    fn m3_rejects_a_modified_rsn_ie() {
        let (mut handshake, anonce, _) = handshake_after_m1();
        let mut invalid_rsn = crate::lmac::RSN_IE_CCMP_PSK;
        invalid_rsn[17] ^= 1;
        let m3 = m3_with_rsn(&handshake, &anonce, &invalid_rsn);

        assert!(matches!(handshake.process(&m3), Err(WpaError::Rsn)));
    }

    #[test]
    fn rsn_validation_accepts_ap_capabilities_but_rejects_tkip() {
        let mut ap_rsn = crate::lmac::RSN_IE_CCMP_PSK;
        ap_rsn[20..22].copy_from_slice(&0x000cu16.to_le_bytes());
        assert!(validate_rsn_ie(&ap_rsn).is_ok());

        ap_rsn[8..14].copy_from_slice(&[1, 0, 0x00, 0x0f, 0xac, 2]);
        assert_eq!(validate_rsn_ie(&ap_rsn), Err(WpaError::Rsn));
    }

    #[test]
    fn four_way_handshake_builds_m2_and_m4_with_valid_mics() {
        let (mut handshake, anonce, m2) = handshake_after_m1();
        let ptk = handshake.ptk.as_ref().unwrap();
        let mut authenticated_m2 = m2.clone();
        authenticated_m2[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].fill(0);
        assert_eq!(
            m2[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH],
            compute_mic(&ptk.kck, &authenticated_m2)
        );
        let expected_tk = ptk.tk;
        let kck = ptk.kck;
        let m3 = m3_with_rsn(&handshake, &anonce, &crate::lmac::RSN_IE_CCMP_PSK);

        let keys = match handshake.process(&m3).unwrap() {
            HandshakeAction::InstallKeys(keys) => keys,
            HandshakeAction::SendM2(_) => panic!("M3 must produce key installation"),
        };
        assert_eq!(keys.temporal_key, expected_tk);
        assert_eq!(keys.group_key, (0x10..=0x1f).collect::<Vec<_>>());
        assert_eq!(keys.group_key_index, 1);
        let mut authenticated_m4 = keys.m4.clone();
        authenticated_m4[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].fill(0);
        assert_eq!(
            keys.m4[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH],
            compute_mic(&kck, &authenticated_m4)
        );
    }

    #[test]
    fn m3_rejects_bad_mic_and_replayed_counter() {
        let (mut mic_handshake, anonce, _) = handshake_after_m1();
        let mut bad_mic = m3_with_rsn(&mic_handshake, &anonce, &crate::lmac::RSN_IE_CCMP_PSK);
        bad_mic[MIC_OFFSET] ^= 1;
        assert!(matches!(
            mic_handshake.process(&bad_mic),
            Err(WpaError::Mic)
        ));

        let (mut replay_handshake, anonce, _) = handshake_after_m1();
        let mut replay = m3_with_rsn(&replay_handshake, &anonce, &crate::lmac::RSN_IE_CCMP_PSK);
        replay[9..17].copy_from_slice(&1u64.to_be_bytes());
        assert!(matches!(
            replay_handshake.process(&replay),
            Err(WpaError::ReplayCounter)
        ));
    }

    #[test]
    fn repeated_m1_with_the_same_replay_counter_retransmits_m2() {
        let (mut handshake, anonce, first_m2) = handshake_after_m1();
        let repeated_m1 = build_eapol_key(
            KEY_VERSION_HMAC_SHA1_AES | KEY_PAIRWISE | KEY_ACK,
            &1u64.to_be_bytes(),
            &anonce,
            &[],
        );

        let repeated_m2 = match handshake.process(&repeated_m1).unwrap() {
            HandshakeAction::SendM2(frame) => frame,
            HandshakeAction::InstallKeys(_) => panic!("repeated M1 must retransmit M2"),
        };

        assert_eq!(repeated_m2, first_m2);
    }

    #[test]
    fn handshake_rejects_invalid_m1_and_m3_key_info() {
        let anonce = core::array::from_fn(|index| index as u8);
        let mut m1_handshake = Wpa2Handshake::new(
            decode("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"),
            [0x02, 0, 0, 0, 0, 1],
            [0x02, 0, 0, 0, 0, 2],
            core::array::from_fn(|index| index as u8 + 32),
        );
        let invalid_m1 = build_eapol_key(
            KEY_VERSION_HMAC_SHA1_AES | KEY_PAIRWISE | KEY_ACK | KEY_INSTALL,
            &1u64.to_be_bytes(),
            &anonce,
            &[],
        );
        assert_eq!(
            m1_handshake.process(&invalid_m1).err(),
            Some(WpaError::UnexpectedMessage)
        );

        let (mut m3_handshake, anonce, _) = handshake_after_m1();
        let mut invalid_m3 = m3_with_rsn(&m3_handshake, &anonce, &crate::lmac::RSN_IE_CCMP_PSK);
        let key_info = u16::from_be_bytes([invalid_m3[5], invalid_m3[6]]) & !KEY_SECURE;
        invalid_m3[5..7].copy_from_slice(&key_info.to_be_bytes());
        invalid_m3[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].fill(0);
        let mic = compute_mic(&m3_handshake.ptk.as_ref().unwrap().kck, &invalid_m3);
        invalid_m3[MIC_OFFSET..MIC_OFFSET + MIC_LENGTH].copy_from_slice(&mic);
        assert_eq!(
            m3_handshake.process(&invalid_m3).err(),
            Some(WpaError::UnexpectedMessage)
        );
    }

    #[test]
    fn gtk_kde_rejects_reserved_bytes_overlong_keys_and_duplicates() {
        let valid = [
            0xdd, 22, 0x00, 0x0f, 0xac, 0x01, 0x01, 0x00, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
            0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
        ];
        assert!(parse_gtk_kde(&valid).is_ok());

        let mut reserved = valid;
        reserved[7] = 1;
        assert_eq!(
            parse_gtk_kde(&reserved).err(),
            Some(WpaError::InvalidKeyData)
        );

        let mut overlong = valid.to_vec();
        overlong[1] = 23;
        overlong.push(0x20);
        assert_eq!(
            parse_gtk_kde(&overlong).err(),
            Some(WpaError::InvalidKeyData)
        );

        let mut duplicate = valid.to_vec();
        duplicate.extend_from_slice(&valid);
        assert_eq!(
            parse_gtk_kde(&duplicate).err(),
            Some(WpaError::InvalidKeyData)
        );
    }
}
