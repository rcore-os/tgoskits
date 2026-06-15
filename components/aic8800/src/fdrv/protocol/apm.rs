//! APM (Access Point Manager) 协议实现
//!
//! 用于 AP 模式启动、停止、beacon 设置等。
//!
//! 起 AP 的 SDIO 序列（参照 vendor rwnx_send_apm_start_req，aic8800_sdio）：
//!   1. 构造完整 beacon 帧（mgmt 头 + 固定字段 + SSID/Rates/DSParam + TIM）
//!   2. APM_SET_BEACON_IE_REQ(0x1c08) 把整帧内联 memcpy 下发（SDIO 不走 DMA）
//!   3. APM_START_REQ(0x1c00) 携带真实 bcn_len/tim_oft/tim_len 启动 AP

use alloc::{sync::Arc, vec, vec::Vec};

use crate::fdrv::{
    core::bus::WifiBus,
    protocol::{cmd::send_cmd, lmac_msg::*},
};

/// 构造一个开放网络的 beacon 帧。
///
/// 布局完全对齐 vendor `rwnx_build_bcn`：head（mgmt 头 + 固定字段 + IEs）
/// 之后紧跟 6 字节 TIM IE。返回 `(beacon, tim_oft, tim_len)`，其中
/// `tim_oft == head_len`（TIM 紧接 head），`tim_len == 6`。
///
/// head 内容（手工构造，无 hostapd）：
///   - 802.11 mgmt 头 24B：frame_ctrl(beacon=0x0080) + duration + DA(bcast) + SA + BSSID + seq
///   - 固定字段 12B：timestamp(8) + beacon_interval(2) + capability(2)
///   - SSID IE：EID=0, len, ssid
///   - Supported Rates IE：EID=1, len, rates
///   - DS Param IE：EID=3, len=1, channel
fn build_open_beacon(
    bssid: &[u8; 6],
    ssid: &[u8],
    channel: u8,
    bcn_int: u16,
) -> (Vec<u8>, u16, u8) {
    let mut head: Vec<u8> = Vec::with_capacity(64);

    // ---- 802.11 mgmt 头 (24B) ----
    head.extend_from_slice(&[0x80, 0x00]); // frame control: type=mgmt, subtype=beacon
    head.extend_from_slice(&[0x00, 0x00]); // duration
    head.extend_from_slice(&[0xFF; 6]); // DA: broadcast
    head.extend_from_slice(bssid); // SA = BSSID
    head.extend_from_slice(bssid); // BSSID
    head.extend_from_slice(&[0x00, 0x00]); // seq ctrl (固件可能改写)

    // ---- 固定字段 (12B) ----
    head.extend_from_slice(&[0u8; 8]); // timestamp (固件填)
    head.extend_from_slice(&bcn_int.to_le_bytes()); // beacon interval
    // capability: ESS(bit0) + Short Preamble(bit5)，开放网络不设 Privacy
    head.extend_from_slice(&0x0021u16.to_le_bytes());

    // ---- SSID IE ----
    head.push(0x00); // EID = SSID
    head.push(ssid.len() as u8);
    head.extend_from_slice(ssid);

    // ---- Supported Rates IE (1,2,5.5,11,6,9,12,18 Mbps) ----
    head.push(0x01); // EID = Supported Rates
    head.push(8);
    head.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24]);

    // ---- DS Parameter Set IE (当前信道) ----
    head.push(0x03); // EID = DS Param
    head.push(1);
    head.push(channel);

    let tim_oft = head.len() as u16;

    // ---- TIM IE (6B，对齐 vendor: EID=5,len=4,count=0,dtim=1,bmctl=0,bitmap=0) ----
    let mut beacon = head;
    beacon.extend_from_slice(&[0x05, 0x04, 0x00, 0x01, 0x00, 0x00]);

    (beacon, tim_oft, 6)
}

/// 下发 beacon 帧：APM_SET_BEACON_IE_REQ (0x1c08)。
///
/// struct apm_set_bcn_ie_req（vendor lmac_msg.h，C 自然对齐）：
///   vif_idx(u8)@0, bcn_ie_len(u16)@2(pad1), bcn_ie[512]@4。SDIO 整帧内联 memcpy。
pub fn send_apm_set_beacon_ie_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    beacon: &[u8],
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    assert!(
        beacon.len() <= 512,
        "beacon too large for apm_set_bcn_ie_req"
    );
    // 头部 4 字节 + bcn_ie[512]，固件按固定 sizeof 收取
    let mut param = vec![0u8; 4 + 512];
    param[0] = vif_idx;
    param[2..4].copy_from_slice(&(beacon.len() as u16).to_le_bytes());
    param[4..4 + beacon.len()].copy_from_slice(beacon);

    log::info!(
        "[apm] APM_SET_BEACON_IE_REQ: vif_idx={}, bcn_len={}",
        vif_idx,
        beacon.len()
    );
    send_cmd(bus, APM_SET_BEACON_IE_REQ, TASK_APM, &param, timeout_ms)
}

/// 发送 APM_START_REQ (0x1c00)，携带真实 beacon 元信息。
///
/// `bcn_len`/`tim_oft`/`tim_len` 必须与已通过 APM_SET_BEACON_IE_REQ 下发的
/// beacon 一致，否则固件会拒绝（之前全填 0 导致超时）。
pub fn send_apm_start_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    channel: u8,
    bcn_len: u16,
    tim_oft: u16,
    tim_len: u8,
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    // apm_start_req 真实布局（vendor lmac_mac.h/lmac_msg.h，C 自然对齐，非 packed）：
    //   u8_l/u16_l/u32_l 均为标准类型，按各自边界对齐，结构体按 4 字节对齐。
    //   字段              类型   偏移   说明
    //   basic_rates       13B    0      mac_rateset: length@0, array[12]@1..13
    //   chan.freq         u16    14     13 后 pad 1（u16 需 2 对齐）
    //   chan.band         u8     16
    //   chan.flags        u8     17
    //   chan.tx_power     s8     18     chan 占 14..20
    //   center_freq1      u32    20     19 后 pad 1（u32 需 4 对齐）
    //   center_freq2      u32    24
    //   ch_width          u8     28
    //   bcn_addr          u32    32     29 后 pad 3（u32 需 4 对齐，SDIO 忽略此字段）
    //   bcn_len           u16    36
    //   tim_oft           u16    38
    //   bcn_int           u16    40
    //   flags             u32    44     42 后 pad 2（u32 需 4 对齐）
    //   ctrl_port_etype   u16    48
    //   tim_len           u8     50
    //   vif_idx           u8     51
    //   尾部 pad 到 4 对齐 → sizeof = 52
    const APM_START_REQ_SIZE: usize = 52;
    let mut param = vec![0u8; APM_START_REQ_SIZE];

    // basic_rates: 支持 1,2,5.5,11 Mbps (802.11b basic rates)
    param[0] = 4; // length
    param[1] = 0x82; // 1 Mbps, basic
    param[2] = 0x84; // 2 Mbps, basic
    param[3] = 0x8B; // 5.5 Mbps, basic
    param[4] = 0x96; // 11 Mbps, basic

    // chan (offset 14): freq, band, flags, tx_power
    let freq = CHAN_2G4_FREQS[(channel as usize - 1).min(13)];
    param[14..16].copy_from_slice(&freq.to_le_bytes());
    param[16] = 0; // band: 2.4GHz (PHY_BAND_2G4)
    param[17] = 0; // flags
    param[18] = 20; // tx_power: 20 dBm

    // center_freq1 (offset 20): 与 chan.freq 一致；center_freq2 (offset 24)=0（非 80+80）
    param[20..24].copy_from_slice(&(freq as u32).to_le_bytes());

    // ch_width (offset 28): PHY_CHNL_BW_20
    param[28] = PHY_CHNL_BW_20;

    // bcn_addr (offset 32): 0 (SDIO 走 APM_SET_BEACON_IE_REQ，此字段死值)
    // bcn_len (offset 36): 真实 beacon 长度
    param[36..38].copy_from_slice(&bcn_len.to_le_bytes());
    // tim_oft (offset 38): TIM IE 在 beacon 中的偏移 (== head_len)
    param[38..40].copy_from_slice(&tim_oft.to_le_bytes());
    // bcn_int (offset 40): 100 TU
    param[40..42].copy_from_slice(&100u16.to_le_bytes());

    // flags (offset 44): 0 (开放网络)
    // ctrl_port_ethertype (offset 48): ETH_P_PAE
    param[48..50].copy_from_slice(&0x888Eu16.to_be_bytes());

    // tim_len (offset 50): TIM IE 总长度 (6)
    param[50] = tim_len;

    // vif_idx (offset 51)
    param[51] = vif_idx;

    log::info!(
        "[apm] APM_START_REQ: vif_idx={}, ch={}, freq={}, bcn_len={}, tim_oft={}, tim_len={}",
        vif_idx,
        channel,
        freq,
        bcn_len,
        tim_oft,
        tim_len
    );

    send_cmd(bus, APM_START_REQ, TASK_APM, &param, timeout_ms)
}

/// 启动一个开放网络 softAP（完整 SDIO 序列）。
///
/// 串起 vendor 在 SDIO 下的启动流程：构造 beacon → APM_SET_BEACON_IE_REQ →
/// APM_START_REQ（带真实 bcn 元信息）。调用前需已完成 MM_ADD_IF(MM_AP) /
/// MM_START / set_filter，且 `vif_idx`/`bssid` 为该 AP 接口的值。
///
/// 返回 APM_START_CFM 的 param（首字节为 status，0 表示成功）。
pub fn start_open_ap(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    bssid: &[u8; 6],
    ssid: &[u8],
    channel: u8,
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    let (beacon, tim_oft, tim_len) = build_open_beacon(bssid, ssid, channel, 100);
    log::info!(
        "[apm] start_open_ap: ssid={:?}, ch={}, beacon_len={}, tim_oft={}",
        core::str::from_utf8(ssid).unwrap_or("<non-utf8>"),
        channel,
        beacon.len(),
        tim_oft
    );

    // 1. 先下发完整 beacon
    send_apm_set_beacon_ie_req(bus, vif_idx, &beacon, timeout_ms)?;

    // 2. 启动 AP，携带真实 bcn 元信息
    send_apm_start_req(
        bus,
        vif_idx,
        channel,
        beacon.len() as u16,
        tim_oft,
        tim_len,
        timeout_ms,
    )
}

/// 发送 APM_STOP_REQ
pub fn send_apm_stop_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    let param = [vif_idx];
    send_cmd(bus, APM_STOP_REQ, TASK_APM, &param, timeout_ms)
}

/// 向固件注册一个已关联的 STA (AP 模式)。返回固件分配的 sta_idx。
///
/// 字段偏移实测自 vendor me_sta_add_req (sizeof=136)。开放/基础 STA：
/// 填 mac + 速率集 + aid + vif_idx，HT/VHT/HE cap 全 0。
/// `rates` 为 802.11 SupportedRates IE 的原始字节(不含 EID/len)。
pub fn send_me_sta_add_req(
    bus: &Arc<WifiBus>,
    sta_mac: &[u8; 6],
    rates: &[u8],
    aid: u16,
    vif_idx: u8,
    timeout_ms: u64,
) -> Result<u8, CmdError> {
    let mut param = vec![0u8; ME_STA_ADD_REQ_SIZE];

    // mac_addr @0 (6)
    param[0..6].copy_from_slice(sta_mac);

    // rate_set @6: length(1) + array[12]
    let n = rates.len().min(12);
    param[6] = n as u8;
    param[7..7 + n].copy_from_slice(&rates[..n]);

    // flags @120 (u32): QoS capable (大多数手机带 WMM)
    param[120..124].copy_from_slice(&STA_QOS_CAPA.to_le_bytes());
    // aid @124 (u16)
    param[124..126].copy_from_slice(&aid.to_le_bytes());
    // uapsd_queues @126, max_sp_len @127 = 0
    // opmode @128 = 0
    // vif_idx @129
    param[129] = vif_idx;

    let cfm = send_cmd(bus, ME_STA_ADD_REQ, TASK_ME, &param, timeout_ms)?;

    // me_sta_add_cfm: sta_idx@0, status@1, pm_state@2
    if cfm.len() < 2 {
        log::warn!("[apm] ME_STA_ADD_CFM too short: {}", cfm.len());
        return Err(CmdError::InvalidResponse);
    }
    let sta_idx = cfm[0];
    let status = cfm[1];
    log::info!(
        "[apm] ME_STA_ADD_CFM: sta_idx={} status={}",
        sta_idx,
        status
    );
    if status != 0 {
        return Err(CmdError::InvalidResponse);
    }
    Ok(sta_idx)
}
