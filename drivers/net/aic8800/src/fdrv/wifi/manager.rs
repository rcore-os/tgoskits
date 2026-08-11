//! WiFi 管理模块
//!
//! 提供扫描、连接、断连的高层 API。
//! AIC8800 是 FullMAC 架构，固件内部处理 802.11 认证/关联，
//! WPA2 四次握手由固件透传 EAPOL 帧（通过 CONTROL_PORT_HOST 标志）。
//! 但在最小移植中，我们让固件自行处理 EAPOL（不设 CONTROL_PORT_HOST），
//! 这样固件会自动完成 WPA2 握手并安装密钥。

extern crate alloc;
use alloc::{sync::Arc, vec::Vec};

use crate::fdrv::{
    core::bus::WifiBus,
    protocol::{
        collect_scan_results, lmac_msg::*, send_key_add_req, send_key_del_req,
        send_scanu_start_req, send_set_control_port_req, send_sm_connect_req,
        send_sm_disconnect_req, wait_for_indication,
    },
};

// ================================================================
// 扫描
// ================================================================

/// 执行 WiFi 扫描
///
/// 流程：
///   1. 发送 SCANU_START_REQ，等待 SCANU_START_CFM_ADDTIONAL
///   2. 扫描期间，固件发送多个 SCANU_RESULT_IND → 路由到 ind_queue
///   3. CFM 返回后，从 ind_queue 收集所有 SCANU_RESULT_IND 并解析
///
/// 参数：
///   - `bus`: WifiBus 引用
///   - `vif_idx`: VIF 索引（通常为 0）
///   - `ssid`: 可选的目标 SSID（None = 广播扫描）
///   - `timeout_ms`: 超时（建议 15000-20000ms）
pub fn scan(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    ssid: Option<&[u8]>,
    timeout_ms: u64,
) -> Result<Vec<ScanResult>, CmdError> {
    {
        let mut queue = bus.tx.ind_queue.lock();
        queue.clear();
    }

    let _cfm = send_scanu_start_req(bus, vif_idx, ssid, timeout_ms)?;

    let results = collect_scan_results(bus, timeout_ms);

    log::debug!("[wifi_mgr] scan complete, {} APs found", results.len());
    Ok(results)
}

/// 在扫描结果中查找指定 SSID 的 AP
pub fn find_ap_by_ssid<'a>(
    results: &'a [ScanResult],
    target_ssid: &[u8],
) -> Option<&'a ScanResult> {
    results.iter().find(|ap| {
        let len = ap.ssid_len as usize;
        len == target_ssid.len() && ap.ssid[..len] == *target_ssid
    })
}

// ================================================================
// 连接
// ================================================================

/// 连接到指定 AP（WPA2-PSK）
///
/// 流程：
///   1. 发送 SM_CONNECT_REQ → SM_CONNECT_CFM（确认收到）
///   2. 等待 SM_CONNECT_IND（异步 indication，包含连接结果）
///   3. 如果 status_code == 0，连接成功
///   4. 发送 ME_SET_CONTROL_PORT_REQ 打开控制端口
///
/// 注意：在最小移植中，不设 CONTROL_PORT_HOST 标志，
/// 让固件自行处理 WPA2 四次握手。驱动只需等待 SM_CONNECT_IND。
///
/// 参数：
///   - `ssid`: AP 的 SSID
///   - `bssid`: AP 的 BSSID
///   - `channel_freq`: AP 的信道频率（MHz），0xFFFF 表示不指定
///   - `password`: WPA2 密码（PSK）— 注意：FullMAC 模式下密码通过 IE 传递
///   - `vif_idx`: VIF 索引
pub fn connect(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    ssid: &[u8],
    bssid: &[u8; 6],
    channel_freq: u16,
    wpa2_ie: &[u8],
    timeout_ms: u64,
) -> Result<ConnectResult, CmdError> {
    log::debug!(
        "[wifi_mgr] connect: ssid_len={}, bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, freq={}",
        ssid.len(),
        bssid[0],
        bssid[1],
        bssid[2],
        bssid[3],
        bssid[4],
        bssid[5],
        channel_freq
    );

    // 清空 ind_queue
    {
        let mut queue = bus.tx.ind_queue.lock();
        queue.clear();
    }

    // 构造 flags
    // WPA_WPA2_IN_USE: 使用 WPA/WPA2
    // 不设 CONTROL_PORT_HOST: 让固件自行处理 EAPOL
    // let flags: u32 = WPA_WPA2_IN_USE | CONTROL_PORT_HOST;
    let flags: u32 = if wpa2_ie.is_empty() {
        0
    } else {
        WPA_WPA2_IN_USE | CONTROL_PORT_HOST | CONTROL_PORT_NO_ENC
    };

    // 发送 SM_CONNECT_REQ
    let cfm = send_sm_connect_req(
        bus,
        vif_idx,
        ssid,
        bssid,
        channel_freq,
        flags,
        WLAN_AUTH_OPEN, // WPA2-PSK 使用 Open System Authentication
        wpa2_ie,
        5000,
    )?;

    // SM_CONNECT_CFM: status(u8)
    if !cfm.is_empty() && cfm[0] != 0 {
        log::error!("[wifi_mgr] SM_CONNECT_CFM status={} (rejected)", cfm[0]);
        return Err(CmdError::FirmwareError);
    }

    log::debug!("[wifi_mgr] SM_CONNECT_CFM OK, waiting for SM_CONNECT_IND...");

    // 等待 SM_CONNECT_IND（异步 indication）
    let ind = wait_for_indication(
        bus,
        SM_CONNECT_IND,
        &[SM_DISCONNECT_IND, SM_EXTERNAL_AUTH_REQUIRED_IND],
        timeout_ms,
    )?;

    // 解析 SM_CONNECT_IND
    let result = parse_connect_ind(&ind)?;

    if result.status_code != 0 {
        log::error!(
            "[wifi_mgr] SM_CONNECT_IND: connection failed, status_code={}",
            result.status_code
        );
        return Err(CmdError::FirmwareError);
    }

    log::debug!(
        "[wifi_mgr] SM_CONNECT_IND: ap_idx={}, ch_idx={}, aid={}",
        result.ap_idx,
        result.ch_idx,
        result.aid
    );

    // WPA2 模式下不在此处打开控制端口
    // 控制端口应在 WPA2 四次握手完成、密钥安装后由调用者打开
    if wpa2_ie.is_empty() {
        // 开放网络：直接打开控制端口
        send_set_control_port_req(bus, result.ap_idx, true, 5000)?;
        log::debug!(
            "[wifi_mgr] open network: control port opened for sta_idx={}",
            result.ap_idx
        );
    } else {
        // WPA2：控制端口由调用者在握手完成后打开
        log::debug!("[wifi_mgr] WPA2: waiting for handshake before opening control port");
    }

    Ok(result)
}

/// 解析 SM_CONNECT_IND 的 param 部分
fn parse_connect_ind(param: &[u8]) -> Result<ConnectResult, CmdError> {
    // sm_connect_ind 布局（参考 lmac_msg.h:2444-2477）:
    //   [0..2]    u16  status_code
    //   [2..8]    mac_addr bssid (6 bytes)
    //   [8]       bool roamed
    //   [9]       u8   vif_idx
    //   [10]      u8   ap_idx
    //   [11]      u8   ch_idx
    //   [12]      bool qos
    //   [13]      u8   acm
    //   [14..16]  u16  assoc_req_ie_len
    //   [16..18]  u16  assoc_rsp_ie_len
    //   [18..20]  2B padding (u32[] alignment)
    //   [20..820] u32  assoc_ie_buf[200] (800 bytes)
    //   [820..822] u16 aid

    if param.len() < 20 {
        log::error!("[wifi_mgr] SM_CONNECT_IND too short: {} bytes", param.len());
        return Err(CmdError::InvalidResponse);
    }

    let status_code = u16::from_le_bytes([param[0], param[1]]);

    let mut bssid = [0u8; 6];
    bssid.copy_from_slice(&param[2..8]);

    let vif_idx = param[9];
    let ap_idx = param[10];
    let ch_idx = param[11];
    let qos = param[12] != 0;

    let assoc_req_ie_len = u16::from_le_bytes([param[14], param[15]]) as usize;
    let assoc_rsp_ie_len = u16::from_le_bytes([param[16], param[17]]) as usize;

    // aid 在 2B padding + 800B assoc_ie_buf 之后，偏移 820
    let aid = if param.len() >= 822 {
        u16::from_le_bytes([param[820], param[821]])
    } else {
        log::warn!(
            "[wifi_mgr] SM_CONNECT_IND too short for aid field ({} bytes), defaulting to 0",
            param.len()
        );
        0
    };

    log::debug!(
        "[wifi_mgr] SM_CONNECT_IND param_len={}, status={}, \
         bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        param.len(),
        status_code,
        bssid[0],
        bssid[1],
        bssid[2],
        bssid[3],
        bssid[4],
        bssid[5]
    );

    // ===== 提取 Association Request IEs =====
    // assoc_ie_buf 从 offset 20 开始，前 assoc_req_ie_len 字节是 AssocReq IEs
    // 后 assoc_rsp_ie_len 字节是 AssocRsp IEs（与 Linux rwnx_msg_rx.c:821-823 一致）
    const IE_BUF_OFFSET: usize = 20;
    let assoc_req_ies = if param.len() >= IE_BUF_OFFSET + assoc_req_ie_len && assoc_req_ie_len > 0 {
        let ies = param[IE_BUF_OFFSET..IE_BUF_OFFSET + assoc_req_ie_len].to_vec();

        // 搜索 RSN IE (tag=0x30) 并打印
        let mut offset = 0;
        while offset + 2 <= ies.len() {
            let tag = ies[offset];
            let len = ies[offset + 1] as usize;
            if offset + 2 + len > ies.len() {
                break;
            }
            if tag == 0x30 {
                let rsn = &ies[offset..offset + 2 + len];
                log::debug!(
                    "[wifi_mgr] >>> AssocReq RSN IE (firmware sent): {:02x?}",
                    rsn
                );
                break;
            }
            offset += 2 + len;
        }

        ies
    } else {
        log::warn!(
            "[wifi_mgr] Cannot extract AssocReq IEs: param_len={}, ie_buf_offset={}, \
             assoc_req_ie_len={}, assoc_rsp_ie_len={}",
            param.len(),
            IE_BUF_OFFSET,
            assoc_req_ie_len,
            assoc_rsp_ie_len
        );
        Vec::new()
    };

    Ok(ConnectResult {
        status_code,
        bssid,
        vif_idx,
        ap_idx,
        ch_idx,
        qos,
        aid,
        assoc_req_ies,
    })
}

// ================================================================
// 密钥安装
// ================================================================

/// 安装 PTK（Pairwise Transient Key）
///
/// 在 WPA2 四次握手完成后调用（如果由驱动处理握手），
/// 或者在 FullMAC 模式下由固件自动完成。
///
/// - `cipher`: MAC_CIPHER_CCMP (3) 或 MAC_CIPHER_TKIP (2)
/// - `key`: 密钥材料（CCMP: 16 bytes, TKIP: 32 bytes）
pub fn install_pairwise_key(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    sta_idx: u8,
    cipher: u8,
    key: &[u8],
    key_idx: u8,
) -> Result<u8, CmdError> {
    log::debug!(
        "[wifi_mgr] install pairwise key: vif={}, sta={}, cipher={}, key_len={}",
        vif_idx,
        sta_idx,
        cipher,
        key.len()
    );

    let hw_key_idx = send_key_add_req(bus, vif_idx, sta_idx, true, key, key_idx, cipher, 5000)?;
    log::debug!(
        "[wifi_mgr] pairwise key installed, hw_key_idx={}",
        hw_key_idx
    );
    Ok(hw_key_idx)
}

/// 安装 GTK
pub fn install_group_key(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    sta_idx: u8,
    cipher: u8,
    key: &[u8],
    key_idx: u8,
) -> Result<u8, CmdError> {
    log::debug!(
        "[wifi_mgr] install group key: vif={}, sta={}, cipher={}, key_idx={}, key_len={}",
        vif_idx,
        sta_idx,
        cipher,
        key_idx,
        key.len()
    );

    let hw_key_idx = send_key_add_req(bus, vif_idx, sta_idx, false, key, key_idx, cipher, 5000)?;
    log::debug!("[wifi_mgr] group key installed, hw_key_idx={}", hw_key_idx);
    Ok(hw_key_idx)
}

/// 删除密钥
pub fn delete_key(bus: &Arc<WifiBus>, hw_key_idx: u8) -> Result<(), CmdError> {
    log::debug!("[wifi_mgr] delete key: hw_key_idx={}", hw_key_idx);
    send_key_del_req(bus, hw_key_idx, 5000)?;
    Ok(())
}

// ================================================================
// 断连
// ================================================================

/// 主动断连
///
/// - `vif_idx`: VIF 索引
/// - `reason_code`: IEEE 802.11 reason code（通常 3 = DEAUTH_LEAVING）
pub fn disconnect(bus: &Arc<WifiBus>, vif_idx: u8, reason_code: u16) -> Result<(), CmdError> {
    log::debug!(
        "[wifi_mgr] disconnect: vif_idx={}, reason_code={}",
        vif_idx,
        reason_code
    );

    let cfm = send_sm_disconnect_req(bus, vif_idx, reason_code, 5000)?;
    if !cfm.is_empty() && cfm[0] != 0 {
        log::warn!("[wifi_mgr] SM_DISCONNECT_CFM status={}", cfm[0]);
    }

    match wait_for_indication(bus, SM_DISCONNECT_IND, &[], 5000) {
        Ok(ind) => {
            if ind.len() >= 4 {
                let reason = u16::from_le_bytes([ind[0], ind[1]]);
                let vif = ind[2];
                let ft_over_ds = ind[3];
                log::debug!(
                    "[wifi_mgr] SM_DISCONNECT_IND: reason={}, vif={}, ft_over_ds={}",
                    reason,
                    vif,
                    ft_over_ds
                );
            }
        }
        Err(CmdError::Timeout) => {
            log::warn!(
                "[wifi_mgr] SM_DISCONNECT_IND timeout (disconnect may still have succeeded)"
            );
        }
        Err(e) => {
            log::error!("[wifi_mgr] SM_DISCONNECT_IND error: {:?}", e);
            return Err(e);
        }
    }
    log::debug!("[wifi_mgr] disconnected");
    Ok(())
}

/// 处理被动断连（由 indication 分发器调用）
///
/// 当 RX 线程收到 SM_DISCONNECT_IND 时调用此函数
pub fn handle_disconnect_ind(param: &[u8]) {
    if param.len() >= 4 {
        let reason = u16::from_le_bytes([param[0], param[1]]);
        let vif_idx = param[2];
        log::warn!(
            "[wifi_mgr] passive disconnect: vif={}, reason={}",
            vif_idx,
            reason
        );
    } else {
        log::warn!(
            "[wifi_mgr] passive disconnect (short param: {} bytes)",
            param.len()
        );
    }
    // TODO: 清理状态、通知上层、可选自动重连
}

// ================================================================
// 构造 WPA2 RSN IE（用于 SM_CONNECT_REQ）
// ================================================================

/// 构造最小的 WPA2-PSK RSN IE
///
/// RSN IE 格式:
///   Tag Number: 48 (0x30)
///   Tag Length: 20
///   Version: 1
///   Group Cipher Suite: 00-0F-AC-04 (CCMP)
///   Pairwise Cipher Suite Count: 1
///   Pairwise Cipher Suite: 00-0F-AC-04 (CCMP)
///   AKM Suite Count: 1
///   AKM Suite: 00-0F-AC-02 (PSK)
///   RSN Capabilities: 0x0000
pub fn build_wpa2_rsn_ie() -> Vec<u8> {
    let mut ie = Vec::with_capacity(22);
    ie.push(0x30); // Element ID: RSN
    ie.push(20); // Length
    ie.extend_from_slice(&1u16.to_le_bytes()); // Version: 1

    // Group Cipher Suite: 00-0F-AC-04 (CCMP)
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);

    // Pairwise Cipher Suite Count: 1
    ie.extend_from_slice(&1u16.to_le_bytes());
    // Pairwise Cipher Suite: 00-0F-AC-04 (CCMP)
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);

    // AKM Suite Count: 1
    ie.extend_from_slice(&1u16.to_le_bytes());
    // AKM Suite: 00-0F-AC-02 (PSK)
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]);

    // RSN Capabilities: 0x0000
    ie.extend_from_slice(&0u16.to_le_bytes());

    ie
}

/// 根据 AP 的 RSN IE 构造 STA 的 RSN IE
/// ap_rsn_ie: AP beacon 中的 RSN IE（包含 tag + length 头部）
pub fn build_wpa2_rsn_ie_from_ap(ap_rsn_ie: &[u8]) -> Vec<u8> {
    // 默认 fallback: TKIP group + CCMP pairwise（最常见的混合模式）
    let group_cipher = if ap_rsn_ie.len() >= 8 {
        // ap_rsn_ie[0]=0x30, [1]=len, [2..4]=version, [4..8]=group cipher
        &ap_rsn_ie[4..8]
    } else {
        // 无法解析 AP RSN IE，默认 TKIP（更兼容）
        &[0x00, 0x0F, 0xAC, 0x02]
    };

    let rsn_cap = [0x00, 0x00]; // 始终使用 0x0000，避免固件修改导致不匹配

    let mut ie = Vec::with_capacity(22);
    ie.push(0x30); // Element ID: RSN
    ie.push(20); // Length
    ie.extend_from_slice(&1u16.to_le_bytes()); // Version: 1

    // Group Cipher Suite: 从 AP 的 RSN IE 中复制
    ie.extend_from_slice(group_cipher);

    // Pairwise Cipher Suite Count: 1
    ie.extend_from_slice(&1u16.to_le_bytes());
    // Pairwise Cipher Suite: CCMP
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x04]);

    // AKM Suite Count: 1
    ie.extend_from_slice(&1u16.to_le_bytes());
    // AKM Suite: PSK
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 0x02]);

    // RSN Capabilities: 0x0000
    ie.extend_from_slice(&rsn_cap);

    ie
}
