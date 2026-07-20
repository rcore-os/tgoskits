//! WPA2-PSK 四次握手实现
//!
//! AIC8800 是 FullMAC 芯片，固件处理 802.11 认证/关联，
//! 但 WPA2 四次握手需要由主机（驱动）完成。
//!
//! 流程：
//!   1. SM_CONNECT_REQ 设置 CONTROL_PORT_HOST | WPA_WPA2_IN_USE
//!   2. 固件完成 802.11 关联后发送 SM_CONNECT_IND
//!   3. AP 发送 EAPOL M1 → 固件作为 DATA 帧转发给主机
//!   4. 主机处理四次握手（M1→M2→M3→M4）
//!   5. 主机通过 MM_KEY_ADD_REQ 安装 PTK 和 GTK
//!   6. 主机通过 ME_SET_CONTROL_PORT_REQ 打开控制端口

extern crate alloc;

use alloc::{vec, vec::Vec};

use aes::{
    Aes128,
    cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray},
};
use hmac::{Hmac, Mac};
use sha1::Sha1;

/// EAPOL 版本 (802.1X-2004)
const EAPOL_VERSION: u8 = 0x01;

/// EAPOL 类型
const EAPOL_TYPE_KEY: u8 = 0x03;

/// Key descriptor type (RSN = 2)
const KEY_DESC_TYPE_RSN: u8 = 0x02;

/// Key Info 位域
const KEY_INFO_TYPE_HMAC_SHA1_AES: u16 = 0x0002; // Key Descriptor Version 2
const KEY_INFO_PAIRWISE: u16 = 0x0008;
const KEY_INFO_INSTALL: u16 = 0x0040;
const KEY_INFO_ACK: u16 = 0x0080;
const KEY_INFO_MIC: u16 = 0x0100;
const KEY_INFO_SECURE: u16 = 0x0200;
const KEY_INFO_ENC_KEY_DATA: u16 = 0x1000;

/// 802.1X header 大小: version(1) + type(1) + body_len(2) = 4
const EAPOL_HDR_LEN: usize = 4;
/// EAPOL-Key body 固定头部大小（不含 Key Data）:
///   desc_type(1) + key_info(2) + key_len(2) + replay(8) + nonce(32) +
///   iv(16) + rsc(8) + reserved(8) + mic(16) + data_len(2) = 95
const EAPOL_KEY_HDR_LEN: usize = 95;
/// MIC 在 EAPOL 帧中的偏移 = EAPOL_HDR_LEN + 77 = 81
const MIC_OFFSET: usize = EAPOL_HDR_LEN + 77;

/// PMK 长度
const PMK_LEN: usize = 32;
/// PTK 长度 (KCK + KEK + TK = 16 + 16 + 16 = 48)
const PTK_LEN: usize = 48;
/// KCK 长度 (Key Confirmation Key)
const KCK_LEN: usize = 16;
/// KEK 长度 (Key Encryption Key)
const KEK_LEN: usize = 16;
/// TK 长度 (Temporal Key, for CCMP)
const TK_LEN: usize = 16;
/// Nonce 长度
const NONCE_LEN: usize = 32;
/// MIC 长度
const MIC_LEN: usize = 16;
/// Replay counter 长度
const REPLAY_COUNTER_LEN: usize = 8;
/// SHA1 digest size
const SHA1_DIGEST_SIZE: usize = 20;

type HmacSha1 = Hmac<Sha1>;

// ================================================================
// 类型定义
// ================================================================

/// 握手状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeState {
    /// 等待 M1
    Idle,
    /// 已发送 M2，等待 M3
    M2Sent,
    /// 握手完成
    Completed,
}

/// 握手动作（process_eapol 的返回值）
pub enum HandshakeAction {
    /// 需要发送 M2 给 AP
    SendM2(Vec<u8>),
    /// 握手完成，包含 M4 帧和密钥材料
    Completed(HandshakeResult),
}

/// 握手完成后的结果
#[derive(Debug, Clone)]
pub struct HandshakeResult {
    /// M4 EAPOL 帧（需要发送给 AP）
    pub m4_frame: Vec<u8>,
    /// Temporal Key（16 字节，用于 CCMP 数据加密）
    pub tk: [u8; TK_LEN],
    /// Group Temporal Key（用于组播/广播解密）
    pub gtk: Vec<u8>,
    /// GTK 的 Key Index
    pub gtk_key_idx: u8,
}

/// WPA2 错误类型
#[derive(Debug)]
pub enum WpaError {
    FrameTooShort,
    InvalidEapolType,
    InvalidDescriptorType,
    UnexpectedMessage,
    InvalidState,
    ReplayCounterMismatch,
    MicMismatch,
    InvalidKeyData,
    AesUnwrapFailed,
    GtkNotFound,
}

impl core::fmt::Display for WpaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WpaError::FrameTooShort => write!(f, "frame too short"),
            WpaError::InvalidEapolType => write!(f, "not an EAPOL-Key frame"),
            WpaError::InvalidDescriptorType => write!(f, "invalid key descriptor type"),
            WpaError::UnexpectedMessage => write!(f, "unexpected message"),
            WpaError::InvalidState => write!(f, "invalid handshake state"),
            WpaError::ReplayCounterMismatch => write!(f, "replay counter mismatch"),
            WpaError::MicMismatch => write!(f, "MIC verification failed"),
            WpaError::InvalidKeyData => write!(f, "invalid key data"),
            WpaError::AesUnwrapFailed => write!(f, "AES key unwrap failed"),
            WpaError::GtkNotFound => write!(f, "GTK not found in key data"),
        }
    }
}

// ================================================================
// EAPOL-Key 帧解析
// ================================================================

/// EAPOL-Key 帧解析结果
#[derive(Debug)]
struct EapolKeyHeader {
    key_info: u16,
    replay_counter: [u8; REPLAY_COUNTER_LEN],
    key_nonce: [u8; NONCE_LEN],
    key_mic: [u8; MIC_LEN],
    key_data: Vec<u8>,
}

/// 解析 EAPOL-Key 帧
///
/// `eapol` 是完整的 EAPOL 帧（从 version 字段开始）
fn parse_eapol_key_header(eapol: &[u8]) -> Result<EapolKeyHeader, WpaError> {
    // 最小长度: EAPOL header (4) + EAPOL-Key header (95) = 99
    if eapol.len() < EAPOL_HDR_LEN + EAPOL_KEY_HDR_LEN {
        return Err(WpaError::FrameTooShort);
    }

    // 检查 EAPOL type
    if eapol[1] != EAPOL_TYPE_KEY {
        return Err(WpaError::InvalidEapolType);
    }

    let off = EAPOL_HDR_LEN; // 4

    // 检查 Key Descriptor Type
    if eapol[off] != KEY_DESC_TYPE_RSN {
        return Err(WpaError::InvalidDescriptorType);
    }

    let key_info = u16::from_be_bytes([eapol[off + 1], eapol[off + 2]]);
    let mut replay_counter = [0u8; REPLAY_COUNTER_LEN];
    replay_counter.copy_from_slice(&eapol[off + 5..off + 13]);

    let mut key_nonce = [0u8; NONCE_LEN];
    key_nonce.copy_from_slice(&eapol[off + 13..off + 45]);

    // reserved: off+69..off+77 (skip)

    let mut key_mic = [0u8; MIC_LEN];
    key_mic.copy_from_slice(&eapol[off + 77..off + 93]);

    let key_data_len = u16::from_be_bytes([eapol[off + 93], eapol[off + 94]]);

    let key_data_start = EAPOL_HDR_LEN + EAPOL_KEY_HDR_LEN; // 99
    let key_data_end = key_data_start + key_data_len as usize;

    if eapol.len() < key_data_end {
        return Err(WpaError::FrameTooShort);
    }

    let key_data = eapol[key_data_start..key_data_end].to_vec();

    Ok(EapolKeyHeader {
        key_info,
        replay_counter,
        key_nonce,
        key_mic,
        key_data,
    })
}

// ================================================================
// PTK 结构体
// ================================================================

#[derive(Clone)]
struct Ptk {
    kck: [u8; KCK_LEN],
    kek: [u8; KEK_LEN],
    tk: [u8; TK_LEN],
}

impl Ptk {
    fn from_bytes(ptk_bytes: &[u8; PTK_LEN]) -> Self {
        let mut kck = [0u8; KCK_LEN];
        let mut kek = [0u8; KEK_LEN];
        let mut tk = [0u8; TK_LEN];
        kck.copy_from_slice(&ptk_bytes[0..16]);
        kek.copy_from_slice(&ptk_bytes[16..32]);
        tk.copy_from_slice(&ptk_bytes[32..48]);
        Self { kck, kek, tk }
    }
}

// ================================================================
// 密码学辅助函数
// ================================================================

/// HMAC-SHA1
fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; SHA1_DIGEST_SIZE] {
    let mut mac = <HmacSha1 as Mac>::new_from_slice(key).expect("HMAC key length");
    mac.update(data);
    let result = mac.finalize();
    let mut out = [0u8; SHA1_DIGEST_SIZE];
    out.copy_from_slice(&result.into_bytes());
    out
}

/// PBKDF2-HMAC-SHA1
fn pbkdf2_sha1(passphrase: &[u8], salt: &[u8], iterations: u32, dk_len: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(dk_len);
    let blocks_needed = dk_len.div_ceil(SHA1_DIGEST_SIZE);

    for block_index in 1..=blocks_needed {
        // U1 = HMAC-SHA1(password, salt || INT(block_idx))
        let mut salt_block = Vec::with_capacity(salt.len() + 4);
        salt_block.extend_from_slice(salt);
        salt_block.extend_from_slice(&(block_index as u32).to_be_bytes());

        let mut u = hmac_sha1(passphrase, &salt_block);
        let mut t = u;

        for _ in 1..iterations {
            u = hmac_sha1(passphrase, &u);
            for i in 0..SHA1_DIGEST_SIZE {
                t[i] ^= u[i];
            }
        }
        result.extend_from_slice(&t);
    }

    result.truncate(dk_len);
    result
}

/// IEEE 802.11i PRF-SHA1 (Pseudo-Random Function)
///
/// PRF-384 for PTK derivation (48 bytes = 384 bits)
fn prf_sha1(key: &[u8], label: &[u8], data: &[u8], output_len: usize) -> Vec<u8> {
    let iterations = output_len.div_ceil(SHA1_DIGEST_SIZE);
    let mut result = Vec::with_capacity(iterations * SHA1_DIGEST_SIZE);

    for i in 0..iterations {
        // HMAC-SHA1(key, label || 0x00 || data || counter)
        let mut input = Vec::with_capacity(label.len() + 1 + data.len() + 1);
        input.extend_from_slice(label);
        input.push(0x00); // separator
        input.extend_from_slice(data);
        input.push(i as u8); // counter

        let hash = hmac_sha1(key, &input);
        result.extend_from_slice(&hash);
    }
    result.truncate(output_len);
    result
}

/// 派生 PTK
///
/// PTK = PRF-384(PMK, "Pairwise key expansion", Min(AA,SPA) || Max(AA,SPA) || Min(ANonce,SNonce) || Max(ANonce,SNonce))
fn derive_ptk(
    pmk: &[u8; PMK_LEN],
    aa: &[u8; 6],
    spa: &[u8; 6],
    anonce: &[u8; NONCE_LEN],
    snonce: &[u8; NONCE_LEN],
) -> Ptk {
    // 构造 data: Min(AA,SPA) || Max(AA,SPA) || Min(ANonce,SNonce) || Max(ANonce,SNonce)
    let mut data = [0u8; 6 + 6 + NONCE_LEN + NONCE_LEN]; // 76 bytes

    // MAC 地址排序
    let (min_addr, max_addr) = if aa[..] < spa[..] {
        (aa.as_slice(), spa.as_slice())
    } else {
        (spa.as_slice(), aa.as_slice())
    };
    data[0..6].copy_from_slice(min_addr);
    data[6..12].copy_from_slice(max_addr);

    // Nonce 排序
    let (min_nonce, max_nonce) = if anonce[..] < snonce[..] {
        (anonce.as_slice(), snonce.as_slice())
    } else {
        (snonce.as_slice(), anonce.as_slice())
    };
    data[12..44].copy_from_slice(min_nonce);
    data[44..76].copy_from_slice(max_nonce);

    let ptk_bytes = prf_sha1(pmk, b"Pairwise key expansion", &data, PTK_LEN);
    let mut ptk_arr = [0u8; PTK_LEN];
    ptk_arr.copy_from_slice(&ptk_bytes);
    Ptk::from_bytes(&ptk_arr)
}

/// 计算 MIC (HMAC-SHA1-128, 取前 16 字节)
fn compute_mic(kck: &[u8], eapol_frame: &[u8]) -> [u8; MIC_LEN] {
    let hash = hmac_sha1(kck, eapol_frame);
    let mut mic = [0u8; MIC_LEN];
    mic.copy_from_slice(&hash[..MIC_LEN]);
    mic
}

/// AES Key Unwrap (RFC 3394)
///
/// `kek`: 16-byte Key Encryption Key
/// `wrapped`: wrapped key data (must be multiple of 8 bytes, >= 16 bytes)
/// Returns unwrapped key data (8 bytes shorter than input)
fn aes_key_unwrap(kek: &[u8], wrapped: &[u8]) -> Result<Vec<u8>, WpaError> {
    if wrapped.len() < 16 || !wrapped.len().is_multiple_of(8) {
        return Err(WpaError::AesUnwrapFailed);
    }

    let n = (wrapped.len() / 8) - 1; // number of 64-bit blocks
    let cipher = Aes128::new(GenericArray::from_slice(kek));

    // Initialize
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[0..8]);

    let mut r = Vec::with_capacity(n * 8);
    for i in 0..n {
        r.extend_from_slice(&wrapped[(i + 1) * 8..(i + 2) * 8]);
    }

    // Unwrap: 6 rounds
    for j in (0..6u64).rev() {
        for i in (0..n).rev() {
            let t = (n as u64) * j + (i as u64) + 1;

            // A ^= t
            let t_bytes = t.to_be_bytes();
            for k in 0..8 {
                a[k] ^= t_bytes[k];
            }

            // B = AES-1(KEK, A || R[i])
            let mut block = [0u8; 16];
            block[0..8].copy_from_slice(&a);
            block[8..16].copy_from_slice(&r[i * 8..(i + 1) * 8]);

            let ga = GenericArray::from_mut_slice(&mut block);
            cipher.decrypt_block(ga);

            a.copy_from_slice(&block[0..8]);
            r[i * 8..(i + 1) * 8].copy_from_slice(&block[8..16]);
        }
    }

    // Check IV
    const DEFAULT_IV: [u8; 8] = [0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6, 0xA6];
    if a != DEFAULT_IV {
        log::error!(
            "[wpa2] AES Key Unwrap IV check failed: {:02x?} != {:02x?}",
            a,
            DEFAULT_IV
        );
        return Err(WpaError::AesUnwrapFailed);
    }

    Ok(r)
}

/// Derives a deterministic supplicant nonce from caller-provided entropy.
///
/// Entropy acquisition is an OS capability and deliberately remains outside
/// the portable crypto module. The caller must provide a fresh unpredictable
/// seed for each association attempt.
pub fn derive_snonce(seed: &[u8]) -> [u8; NONCE_LEN] {
    let mut snonce = [0u8; NONCE_LEN];
    let hash1 = hmac_sha1(seed, b"aic8800-snonce-1");
    let hash2 = hmac_sha1(seed, b"aic8800-snonce-2");
    snonce[..20].copy_from_slice(&hash1);
    snonce[20..32].copy_from_slice(&hash2[..12]);
    snonce
}

/// 构造 EAPOL-Key 帧
///
/// 返回完整的 EAPOL 帧（从 version 字段开始），MIC 字段初始化为全零
fn build_eapol_key_frame(
    key_info: u16,
    key_length: u16,
    replay_counter: &[u8; REPLAY_COUNTER_LEN],
    key_nonce: &[u8; NONCE_LEN],
    key_data: &[u8],
) -> Vec<u8> {
    let key_data_len = key_data.len() as u16;
    let body_len = (EAPOL_KEY_HDR_LEN + key_data.len()) as u16;

    let total_len = EAPOL_HDR_LEN + EAPOL_KEY_HDR_LEN + key_data.len();
    let mut frame = vec![0u8; total_len];

    // 802.1X header
    frame[0] = EAPOL_VERSION;
    frame[1] = EAPOL_TYPE_KEY;
    frame[2..4].copy_from_slice(&body_len.to_be_bytes());

    let off = EAPOL_HDR_LEN; // 4

    // Key Descriptor Type
    frame[off] = KEY_DESC_TYPE_RSN;

    // Key Info
    frame[off + 1..off + 3].copy_from_slice(&key_info.to_be_bytes());

    // Key Length
    frame[off + 3..off + 5].copy_from_slice(&key_length.to_be_bytes());

    // Replay Counter
    frame[off + 5..off + 13].copy_from_slice(replay_counter);

    // Key Nonce
    frame[off + 13..off + 45].copy_from_slice(key_nonce);

    // Key IV: [off+45..off+61] = 0 (already zero)
    // Key RSC: [off+61..off+69] = 0 (already zero)
    // Reserved: [off+69..off+77] = 0 (already zero)
    // Key MIC: [off+77..off+93] = 0 (will be filled by caller)

    // Key Data Length
    frame[off + 93..off + 95].copy_from_slice(&key_data_len.to_be_bytes());

    // Key Data
    if !key_data.is_empty() {
        frame[EAPOL_HDR_LEN + EAPOL_KEY_HDR_LEN..].copy_from_slice(key_data);
    }

    frame
}

/// 解析 GTK KDE (Key Data Encapsulation)
///
/// KDE 格式:
///   [0]     type = 0xDD (Vendor Specific)
///   [1]     length
///   [2..5]  OUI + data type = 00-0F-AC-01 (GTK KDE)
///   [6]     Key ID (bits 0-1) | Tx (bit 2)
///   [7]     reserved
///   [8..]   GTK
fn parse_gtk_kde(data: &[u8]) -> Result<(Vec<u8>, u8), WpaError> {
    let mut offset = 0;

    while offset + 2 <= data.len() {
        let element_type = data[offset];
        let element_len = data[offset + 1] as usize;

        if offset + 2 + element_len > data.len() {
            return Err(WpaError::InvalidKeyData);
        }

        // 检查是否是 GTK KDE: type=0xDD, OUI=00-0F-AC, data_type=01
        if element_type == 0xDD
            && element_len >= 6
            && data[offset + 2..offset + 6] == [0x00, 0x0F, 0xAC, 0x01]
        {
            let key_id = data[offset + 6] & 0x03; // Key ID 在 bits 0-1
            let gtk = data[offset + 8..offset + 2 + element_len].to_vec();
            log::debug!(
                "[wpa2] Found GTK KDE: key_id={}, gtk_len={}",
                key_id,
                gtk.len(),
            );
            return Ok((gtk, key_id));
        }

        // 跳过 padding (type=0x00)
        if element_type == 0x00 {
            offset += 1;
            continue;
        }

        offset += 2 + element_len;
    }

    log::error!(
        "[wpa2] GTK KDE not found in key data ({} bytes)",
        data.len()
    );
    Err(WpaError::GtkNotFound)
}

// ================================================================
// WPA2 握手上下文
// ================================================================

pub struct Wpa2Handshake {
    pub state: HandshakeState,
    pmk: [u8; PMK_LEN],
    ptk: Option<Ptk>,
    anonce: [u8; NONCE_LEN],
    snonce: [u8; NONCE_LEN],
    aa: [u8; 6],
    spa: [u8; 6],
    rsn_ie: Vec<u8>,
    replay_counter: [u8; REPLAY_COUNTER_LEN],
    gtk: Vec<u8>,
    gtk_key_idx: u8,
}

impl Wpa2Handshake {
    pub fn new(
        passphrase: &[u8],
        ssid: &[u8],
        aa: &[u8; 6],
        spa: &[u8; 6],
        rsn_ie: &[u8],
        entropy: &[u8],
    ) -> Self {
        let pmk_vec = pbkdf2_sha1(passphrase, ssid, 4096, PMK_LEN);
        let mut pmk = [0u8; PMK_LEN];
        pmk.copy_from_slice(&pmk_vec);

        let snonce = derive_snonce(entropy);
        Self {
            state: HandshakeState::Idle,
            pmk,
            ptk: None,
            snonce,
            anonce: [0u8; NONCE_LEN],
            aa: *aa,
            spa: *spa,
            rsn_ie: rsn_ie.to_vec(),
            replay_counter: [0u8; REPLAY_COUNTER_LEN],
            gtk: Vec::new(),
            gtk_key_idx: 0,
        }
    }

    /// 更新握手使用的 RSN IE（当固件修改了 Association Request 中的 RSN IE 时调用）
    pub fn update_rsn_ie(&mut self, new_rsn_ie: &[u8]) {
        log::debug!(
            "[wpa2] Updating RSN IE: old={:02x?}, new={:02x?}",
            self.rsn_ie,
            new_rsn_ie
        );
        self.rsn_ie = new_rsn_ie.to_vec();
    }

    /// 处理收到的 EAPOL 帧，返回需要执行的动作
    ///
    /// `eapol` 是完整的 EAPOL 帧（从 802.1X Version 字段开始，不含 Ethernet 头）
    pub fn process_eapol(&mut self, eapol: &[u8]) -> Result<HandshakeAction, WpaError> {
        let hdr = parse_eapol_key_header(eapol)?;

        // 判断是 M1 还是 M3
        let has_ack = (hdr.key_info & KEY_INFO_ACK) != 0;
        let has_mic = (hdr.key_info & KEY_INFO_MIC) != 0;
        let has_install = (hdr.key_info & KEY_INFO_INSTALL) != 0;
        let has_enc = (hdr.key_info & KEY_INFO_ENC_KEY_DATA) != 0;

        if has_ack && !has_mic {
            // M1: ACK=1, MIC=0
            log::debug!(
                "[wpa2] === M1 === key_info=0x{:04x} replay={:02x?}",
                hdr.key_info,
                hdr.replay_counter
            );
            self.process_m1(&hdr, eapol)
        } else if has_ack && has_mic && has_install && has_enc {
            // M3: ACK=1, MIC=1, Install=1, EncKeyData=1
            log::debug!("[wpa2] === M3 === key_info=0x{:04x}", hdr.key_info);
            self.process_m3(&hdr, eapol)
        } else {
            log::warn!(
                "[wpa2] Unexpected EAPOL key_info=0x{:04x}, ignoring",
                hdr.key_info
            );
            Err(WpaError::UnexpectedMessage)
        }
    }

    fn process_m1(
        &mut self,
        hdr: &EapolKeyHeader,
        _eapol: &[u8],
    ) -> Result<HandshakeAction, WpaError> {
        if self.state != HandshakeState::Idle && self.state != HandshakeState::M2Sent {
            log::warn!("[wpa2] M1 received in unexpected state: {:?}", self.state);
            // 允许重新开始（AP 可能重发 M1）
        }

        // 保存 ANonce 和 Replay Counter
        self.anonce.copy_from_slice(&hdr.key_nonce);
        self.replay_counter.copy_from_slice(&hdr.replay_counter);

        // 派生 PTK
        let ptk = derive_ptk(&self.pmk, &self.aa, &self.spa, &self.anonce, &self.snonce);

        self.ptk = Some(ptk);

        // 构造 M2
        let key_info: u16 = KEY_INFO_TYPE_HMAC_SHA1_AES | KEY_INFO_PAIRWISE | KEY_INFO_MIC;

        let mut m2 = build_eapol_key_frame(
            key_info,
            0, // key_length = 0 in M2
            &self.replay_counter,
            &self.snonce, // M2 携带 SNonce
            &self.rsn_ie, // Key Data = RSN IE
        );

        // 计算并填入 MIC
        let mic = compute_mic(&self.ptk.as_ref().unwrap().kck, &m2);
        m2[MIC_OFFSET..MIC_OFFSET + MIC_LEN].copy_from_slice(&mic);

        self.state = HandshakeState::M2Sent;
        log::debug!(
            "[wpa2] M2 built ({} bytes), snonce={:02x?}.. anonce={:02x?}.. MIC={:02x?}",
            m2.len(),
            &self.snonce[..4],
            &self.anonce[..4],
            &mic[..4]
        );

        Ok(HandshakeAction::SendM2(m2))
    }

    fn process_m3(
        &mut self,
        hdr: &EapolKeyHeader,
        eapol: &[u8],
    ) -> Result<HandshakeAction, WpaError> {
        if self.state != HandshakeState::M2Sent {
            log::warn!("[wpa2] M3 received in unexpected state: {:?}", self.state);
            return Err(WpaError::InvalidState);
        }

        let ptk = self.ptk.as_ref().ok_or(WpaError::InvalidState)?;

        // 验证 Replay Counter（必须 > 之前的值）
        if hdr.replay_counter[..] < self.replay_counter[..] {
            log::error!("[wpa2] M3 replay counter too old");
            return Err(WpaError::ReplayCounterMismatch);
        }
        self.replay_counter.copy_from_slice(&hdr.replay_counter);

        // 验证 MIC
        let mut eapol_copy = eapol.to_vec();
        // 将 MIC 字段清零后计算
        for i in 0..MIC_LEN {
            eapol_copy[MIC_OFFSET + i] = 0;
        }

        let computed_mic = compute_mic(&ptk.kck, &eapol_copy);
        if computed_mic != hdr.key_mic {
            log::error!(
                "[wpa2] M3 MIC mismatch! expected={:02x?}, got={:02x?}",
                &computed_mic[..4],
                &hdr.key_mic[..4],
            );
            return Err(WpaError::MicMismatch);
        }
        log::debug!("[wpa2] M3 MIC verified OK");

        // 验证 ANonce 一致性
        if hdr.key_nonce != self.anonce {
            log::warn!("[wpa2] M3 ANonce differs from M1, updating");
            self.anonce.copy_from_slice(&hdr.key_nonce);
        }

        // 解密 Key Data（包含 GTK KDE）
        let key_data = &hdr.key_data;
        if key_data.is_empty() {
            log::error!("[wpa2] M3 has no key data");
            return Err(WpaError::InvalidKeyData);
        }

        let decrypted = aes_key_unwrap(&ptk.kek, key_data)?;
        log::debug!("[wpa2] M3 key data decrypted: {} bytes", decrypted.len());

        // 解析 GTK KDE
        let (gtk, gtk_key_idx) = parse_gtk_kde(&decrypted)?;
        self.gtk = gtk;
        self.gtk_key_idx = gtk_key_idx;

        log::debug!(
            "[wpa2] GTK extracted: key_idx={}, len={}",
            self.gtk_key_idx,
            self.gtk.len(),
        );

        // 构造 M4
        let key_info: u16 =
            KEY_INFO_TYPE_HMAC_SHA1_AES | KEY_INFO_PAIRWISE | KEY_INFO_MIC | KEY_INFO_SECURE;

        let mut m4 = build_eapol_key_frame(
            key_info,
            0, // key_length = 0 in M4
            &self.replay_counter,
            &[0u8; NONCE_LEN], // M4 nonce 全零
            &[],               // M4 无 key data
        );

        // 计算并填入 MIC
        let mic = compute_mic(&ptk.kck, &m4);
        m4[MIC_OFFSET..MIC_OFFSET + MIC_LEN].copy_from_slice(&mic);

        self.state = HandshakeState::Completed;
        log::debug!("[wpa2] M4 built ({} bytes), handshake complete!", m4.len());

        // 返回结果
        let mut tk = [0u8; TK_LEN];
        tk.copy_from_slice(&ptk.tk);

        Ok(HandshakeAction::Completed(HandshakeResult {
            m4_frame: m4,
            tk,
            gtk: self.gtk.clone(),
            gtk_key_idx: self.gtk_key_idx,
        }))
    }
}
