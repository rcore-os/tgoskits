use alloc::vec::Vec;

/// LMAC 消息头（对应 Linux struct lmac_msg）
#[repr(C)]
#[derive(Clone, Debug)]
pub struct LmacMsg {
    pub id: u16,
    pub dest_id: u16,
    pub src_id: u16,
    pub param_len: u16,
    pub pattern: u32,
}

impl LmacMsg {
    pub const SIZE: usize = 12;

    /// 从字节切片解析 LmacMsg（小端序）
    pub fn from_le_bytes(data: &[u8]) -> Self {
        Self {
            id: u16::from_le_bytes([data[0], data[1]]),
            dest_id: u16::from_le_bytes([data[2], data[3]]),
            src_id: u16::from_le_bytes([data[4], data[5]]),
            param_len: u16::from_le_bytes([data[6], data[7]]),
            pattern: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
        }
    }
}

/// 构造宏：LMAC_FIRST_MSG(task) = (task << 10)
pub const fn lmac_first_msg(task: u16) -> u16 {
    task << 10
}

/// 从 msg_id 提取 message index: bits[9..0]
pub const fn msg_index(msg_id: u16) -> u16 {
    msg_id & ((1 << 10) - 1)
}

// Task IDs — TASK_DBG 和 DRV_TASK_ID 定义在 aic8800_common，
// 此处导入以确保来源唯一
pub use crate::common::{DRV_TASK_ID, TASK_DBG};

// 其余任务 ID 仅在本模块使用
pub const TASK_MM: u16 = 0;
pub const TASK_SCAN: u16 = 2;
pub const TASK_TDLS: u16 = 3;
pub const TASK_SCANU: u16 = 4;
pub const TASK_ME: u16 = 5;
pub const TASK_SM: u16 = 6;
pub const TASK_APM: u16 = 7;
pub const TASK_BAM: u16 = 8;
pub const TASK_MESH: u16 = 9;
pub const TASK_RXU: u16 = 10;
pub const TASK_RM: u16 = 11;
pub const TASK_TWT: u16 = 12;
pub const TASK_API: u16 = 13;

// ============================================================
// LMAC 消息 ID（TASK_MM = 0, LMAC_FIRST_MSG(0) = 0）
// ============================================================
pub const MM_SET_STACK_START_REQ: u16 = 0x007B; // 枚举偏移 123
pub const MM_SET_STACK_START_CFM: u16 = 0x007C;

// ========== MM messages (TASK_MM = 0, base = 0x0000) ==========
pub const MM_RESET_REQ: u16 = 0x0000;
pub const MM_RESET_CFM: u16 = 0x0001;
pub const MM_START_REQ: u16 = 0x0002;
pub const MM_START_CFM: u16 = 0x0003;
pub const MM_VERSION_REQ: u16 = 0x0004;
pub const MM_VERSION_CFM: u16 = 0x0005;
pub const MM_ADD_IF_REQ: u16 = 0x0006;
pub const MM_ADD_IF_CFM: u16 = 0x0007;
pub const MM_REMOVE_IF_REQ: u16 = 0x0008;
pub const MM_REMOVE_IF_CFM: u16 = 0x0009;
pub const MM_STA_ADD_REQ: u16 = 0x000A;
pub const MM_STA_ADD_CFM: u16 = 0x000B;
pub const MM_STA_DEL_REQ: u16 = 0x000C;
pub const MM_STA_DEL_CFM: u16 = 0x000D;
pub const MM_SET_FILTER_REQ: u16 = 0x000E;
pub const MM_SET_FILTER_CFM: u16 = 0x000F;
pub const MM_SET_CHANNEL_REQ: u16 = 0x0010;
pub const MM_SET_CHANNEL_CFM: u16 = 0x0011;
pub const MM_SET_IDLE_REQ: u16 = 0x0022;
pub const MM_SET_IDLE_CFM: u16 = 0x0023;
pub const MM_KEY_ADD_REQ: u16 = 0x0024;
pub const MM_KEY_ADD_CFM: u16 = 0x0025;
pub const MM_KEY_DEL_REQ: u16 = 0x0026;
pub const MM_KEY_DEL_CFM: u16 = 0x0027;
pub const MM_CHANNEL_SURVEY_IND: u16 = 0x004F;

// RF 校准相关
pub const MM_SET_RF_CONFIG_REQ: u16 = 0x0067; // idx 103
pub const MM_SET_RF_CONFIG_CFM: u16 = 0x0068; // idx 104
pub const MM_SET_RF_CALIB_REQ: u16 = 0x0069; // idx 105
pub const MM_SET_RF_CALIB_CFM: u16 = 0x006A; // idx 106

/// mm_set_rf_calib_req 结构体大小 (v1, 对应 Linux 驱动 AIC8801 非D80X2路径)
///   cal_cfg_24g(4) + cal_cfg_5g(4) + param_alpha(4) + bt_calib_en(4) +
///   bt_calib_param(4) + xtal_cap(1) + xtal_cap_fine(1) = 22 bytes
pub const MM_SET_RF_CALIB_REQ_SIZE: usize = 22;

/// AIC8801 RF 校准: 2.4GHz 校准配置位掩码
/// 对应 Linux: rf_calib_req->cal_cfg_24g = 0xbf (AIC8801)
pub const AIC8801_RF_CAL_CFG_24G: u32 = 0xbf;

/// AIC8801 RF 校准: 5GHz 校准配置位掩码
/// 对应 Linux: rf_calib_req->cal_cfg_5g = 0x3f (AIC8801)
pub const AIC8801_RF_CAL_CFG_5G: u32 = 0x3f;

/// AIC8801 RF 校准: alpha 参数
/// 对应 Linux: rf_calib_req->param_alpha = 0x0c34c008
pub const AIC8801_RF_PARAM_ALPHA: u32 = 0x0c34c008;

/// AIC8801 RF 校准: BT coexistence 校准参数
/// 对应 Linux: rf_calib_req->bt_calib_param = 0x264203
pub const AIC8801_RF_BT_CALIB_PARAM: u32 = 0x264203;

// ===== ME Config =====
// mac_htcapability: ht_capa_info(u16) + a_mpdu_param(u8) + mcs_rate[16](u8) +
//                   ht_extended_capa(u16) + tx_beamforming_capa(u32) + asel_capa(u8) = 26
pub const MAC_HT_CAPABILITY_SIZE: usize = 26;
// mac_vhtcapability: vht_capa_info(u32) + rx_mcs_map(u16) + rx_highest(u16) +
//                    tx_mcs_map(u16) + tx_highest(u16) = 12
pub const MAC_VHT_CAPABILITY_SIZE: usize = 12;
// mac_hecapability: mac_cap_info[6] + phy_cap_info[11] + mcs_supp(4*u16=8) + ppe_thres[25] = 50
// 加上 C 结构体对齐 padding = 54
pub const MAC_HE_CAPABILITY_SIZE: usize = 54;
// me_config_req: ht_cap + vht_cap + he_cap + tail(tx_lft+phy_bw+ht_supp等 10 bytes)
pub const ME_CONFIG_REQ_SIZE: usize =
    MAC_HT_CAPABILITY_SIZE + MAC_VHT_CAPABILITY_SIZE + MAC_HE_CAPABILITY_SIZE + 10; // 102
// HT capability flags
pub const HT_CAPA_INFO_LDPC: u16 = 0x0001;
pub const HT_AMPDU_FACTOR_MAX: u8 = 3;
pub const HT_AMPDU_DENSITY_MAX: u8 = 7;
pub const HT_MCS_RATE_1SS: u8 = 0xFF;
// ME_CONFIG tail 字段偏移 (从 tail 起始处计算)
pub const ME_CONFIG_TAIL_TX_LFT_OFF: usize = 0; // u16, 默认 0
pub const ME_CONFIG_TAIL_PHY_BW_OFF: usize = 2; // u8, PHY_CHNL_BW_20
pub const ME_CONFIG_TAIL_HT_SUPP_OFF: usize = 3; // bool, true

// ===== MM Start =====
// mm_start_req: phy_cfg_tag(16*u32=64) + uapsd_timeout(u32) + lp_clk_accuracy(u16) = 70
pub const MM_START_REQ_SIZE: usize = 70;
pub const MM_START_PHY_CFG_SIZE: usize = 64;
pub const MM_START_UAPSD_TIMEOUT_MS: u32 = 300;
pub const MM_START_LP_CLK_ACCURACY_PPM: u16 = 20;

// ===== MM Add Interface =====
// mm_add_if_req: type(u8) + padding(1) + mac_addr(6) + p2p(bool) + padding(1) = 10
pub const MM_ADD_IF_REQ_SIZE: usize = 10;

// ===== MM Get MAC Address =====
// mm_get_mac_addr_req: get(u32) = 1 表示请求获取
pub const MM_GET_MAC_ADDR_REQ_GET: u32 = 1;

// ===== ME Channel Config =====
pub const ME_CHAN_MAX_2G4: usize = 14;
pub const ME_CHAN_MAX_5G: usize = 28;
pub const ME_CHAN_TX_POWER_DEFAULT: i8 = 30; // 30 dBm

// MAC 地址
pub const MM_GET_MAC_ADDR_REQ: u16 = 0x0073; // idx 115
pub const MM_GET_MAC_ADDR_CFM: u16 = 0x0074; // idx 116

// TX 功率
pub const MM_SET_TXPWR_IDX_LVL_REQ: u16 = 0x0077; // idx 119
pub const MM_SET_TXPWR_IDX_LVL_CFM: u16 = 0x0078; // idx 120
pub const MM_SET_TXPWR_OFST_REQ: u16 = 0x0079; // idx 121
pub const MM_SET_TXPWR_OFST_CFM: u16 = 0x007A; // idx 122

// ========== ME messages (TASK_ME = 5, base = 0x1400) ==========
pub const ME_CONFIG_REQ: u16 = 0x1400;
pub const ME_CONFIG_CFM: u16 = 0x1401;
pub const ME_CHAN_CONFIG_REQ: u16 = 0x1402;
pub const ME_CHAN_CONFIG_CFM: u16 = 0x1403;
pub const ME_SET_CONTROL_PORT_REQ: u16 = 0x1404;
pub const ME_SET_CONTROL_PORT_CFM: u16 = 0x1405;
// ME_TKIP_MIC_FAILURE_IND = 0x1406
pub const ME_STA_ADD_REQ: u16 = 0x1407;
pub const ME_STA_ADD_CFM: u16 = 0x1408;
/// me_sta_add_req 大小 (实测 offsetof，u8_l=uint8_t)
pub const ME_STA_ADD_REQ_SIZE: usize = 136;
/// STA flags (mac_sta_flags)
pub const STA_QOS_CAPA: u32 = 1 << 0;
pub const ME_SET_PS_MODE_REQ: u16 = 0x1413;
pub const ME_SET_PS_MODE_CFM: u16 = 0x1414;

pub const MM_PS_MODE_OFF: u8 = 0;
pub const MM_PS_MODE_ON: u8 = 1;
pub const MM_PS_MODE_ON_DYN: u8 = 2;

// ========== VIF 类型 ==========
pub const MM_STA: u8 = 0;
pub const MM_IBSS: u8 = 1;
pub const MM_AP: u8 = 2;

// ========== PHY BW ==========
pub const PHY_CHNL_BW_20: u8 = 0;
pub const PHY_CHNL_BW_40: u8 = 1;
pub const PHY_CHNL_BW_80: u8 = 2;

/// CMD 超时（与 Linux RWNX_80211_CMD_TIMEOUT_MS 一致）
pub const CMD_TIMEOUT_MS: u64 = 6000;

/// CMD TX 默认超时（当 timeout_ms == 0 时使用）
pub const CMD_TX_TIMEOUT_DEFAULT_MS: u64 = 5000;

#[derive(Debug)]
pub enum CmdError {
    Timeout,
    BusDown,
    SdioError,
    InvalidResponse,
    MismatchedCfm { expected: u16, got: u16 },
    FirmwareError,
}

/// 2.4GHz 标准信道频率表 (信道 1-14)
pub const CHAN_2G4_FREQS: [u16; 14] = [
    2412, 2417, 2422, 2427, 2432, 2437, 2442, 2447, 2452, 2457, 2462, 2467, 2472, 2484,
];

// ========== SCANU messages (TASK_SCANU = 4, base = 0x1000) ==========
pub const SCANU_START_REQ: u16 = lmac_first_msg(TASK_SCANU); // 0x1000
pub const SCANU_START_CFM: u16 = lmac_first_msg(TASK_SCANU) + 1; // 0x1001
pub const SCANU_JOIN_REQ: u16 = lmac_first_msg(TASK_SCANU) + 2; // 0x1002
pub const SCANU_JOIN_CFM: u16 = lmac_first_msg(TASK_SCANU) + 3; // 0x1003
pub const SCANU_RESULT_IND: u16 = lmac_first_msg(TASK_SCANU) + 4; // 0x1004
pub const SCANU_FAST_REQ: u16 = lmac_first_msg(TASK_SCANU) + 5; // 0x1005
pub const SCANU_FAST_CFM: u16 = lmac_first_msg(TASK_SCANU) + 6; // 0x1006
pub const SCANU_VENDOR_IE_REQ: u16 = lmac_first_msg(TASK_SCANU) + 7; // 0x1007
pub const SCANU_VENDOR_IE_CFM: u16 = lmac_first_msg(TASK_SCANU) + 8; // 0x1008
pub const SCANU_START_CFM_ADDTIONAL: u16 = lmac_first_msg(TASK_SCANU) + 9; // 0x1009
pub const SCANU_CANCEL_REQ: u16 = lmac_first_msg(TASK_SCANU) + 10; // 0x100A
pub const SCANU_CANCEL_CFM: u16 = lmac_first_msg(TASK_SCANU) + 11; // 0x100B

// ========== 扫描/地址结构体常量 ==========
/// 最大扫描 SSID 数量
pub const SCAN_SSID_MAX: usize = 3;
/// 最大扫描信道数量 (2.4GHz 14 + 5GHz 28)
pub const SCAN_CHANNEL_MAX: usize = 42;
/// SSID 最大长度 (不含 length 字节)
pub const MAC_SSID_LEN: usize = 32;
/// MAC 地址长度
pub const MAC_ADDR_LEN: usize = 6;

/// 从 msg_id 提取 task_id: bits[15..10]
pub const fn msg_task(id: u16) -> u16 {
    id >> 10
}

/// mac_chan_def 结构体大小: freq(u16) + band(u8) + flags(u8) + tx_power(i8) + padding = 6 bytes
pub const MAC_CHAN_DEF_SIZE: usize = 6;

/// mac_ssid 结构体大小: length(u8) + array([u8;32]) = 33 bytes
pub const MAC_SSID_SIZE: usize = 33;

/// mac_addr 结构体大小: array([u16;3]) = 6 bytes
pub const MAC_ADDR_SIZE: usize = 6;

//   chan[42]:     42 * 6 = 252   (offset 0)
//   ssid[3]:     3 * 33 = 99    (offset 252)
//   bssid:       6              (offset 351)
//   add_ies:     4              (offset 357 → pad to 360)
//   add_ie_len:  2              (offset 364)
//   vif_idx:     1              (offset 366)
//   chan_cnt:     1              (offset 367)
//   ssid_cnt:    1              (offset 368)
//   no_cck:      1              (offset 369)
//   duration:    4              (offset 370 → pad to 372)
//   total:       376
pub const SCANU_START_REQ_SIZE: usize = 376;

/// scanu_start_cfm: 3 bytes (vif_idx:u8 + status:u8 + result_cnt:u8)
/// scanu_result_ind 头部: 10 bytes (length:u16 + framectrl:u16 + center_freq:u16
///                                  + band:u8 + sta_idx:u8 + inst_nbr:u8 + rssi:i8)
///   后跟 payload[] (变长)

// 扫描结果（从 SCANU_RESULT_IND 解析）
#[derive(Clone, Debug)]
pub struct ScanResult {
    pub ssid: [u8; MAC_SSID_LEN],
    pub ssid_len: u8,
    pub bssid: [u8; 6],
    pub center_freq: u16,
    pub rssi: i8,
    pub capability: u16,
    pub beacon_interval: u16,
    /// 原始 802.11 管理帧 payload（用于后续 IE 解析）
    pub raw_payload: Vec<u8>,
    /// AP 的 RSN IE（含 tag=0x30 + length 头部），空表示开放网络
    pub rsn_ie: Vec<u8>,
}

// ================================================================
// 以下为扫描 + 连接 + 密钥管理 + 断连所需的补充定义
// ================================================================

// ========== APM messages (TASK_APM = 7, base = 0x1C00) ==========
pub const APM_START_REQ: u16 = lmac_first_msg(TASK_APM); // 0x1C00
pub const APM_START_CFM: u16 = lmac_first_msg(TASK_APM) + 1; // 0x1C01
pub const APM_STOP_REQ: u16 = lmac_first_msg(TASK_APM) + 2; // 0x1C02
pub const APM_STOP_CFM: u16 = lmac_first_msg(TASK_APM) + 3; // 0x1C03
pub const APM_START_CAC_REQ: u16 = lmac_first_msg(TASK_APM) + 4; // 0x1C04
pub const APM_START_CAC_CFM: u16 = lmac_first_msg(TASK_APM) + 5; // 0x1C05
pub const APM_STOP_CAC_REQ: u16 = lmac_first_msg(TASK_APM) + 6; // 0x1C06
pub const APM_STOP_CAC_CFM: u16 = lmac_first_msg(TASK_APM) + 7; // 0x1C07
pub const APM_SET_BEACON_IE_REQ: u16 = lmac_first_msg(TASK_APM) + 8; // 0x1C08
pub const APM_SET_BEACON_IE_CFM: u16 = lmac_first_msg(TASK_APM) + 9; // 0x1C09

// ========== SM messages (TASK_SM = 6, base = 0x1800) ==========
pub const SM_CONNECT_REQ: u16 = lmac_first_msg(TASK_SM); // 0x1800
pub const SM_CONNECT_CFM: u16 = lmac_first_msg(TASK_SM) + 1; // 0x1801
pub const SM_CONNECT_IND: u16 = lmac_first_msg(TASK_SM) + 2; // 0x1802
pub const SM_DISCONNECT_REQ: u16 = lmac_first_msg(TASK_SM) + 3; // 0x1803
pub const SM_DISCONNECT_CFM: u16 = lmac_first_msg(TASK_SM) + 4; // 0x1804
pub const SM_DISCONNECT_IND: u16 = lmac_first_msg(TASK_SM) + 5; // 0x1805
pub const SM_EXTERNAL_AUTH_REQUIRED_IND: u16 = lmac_first_msg(TASK_SM) + 6; // 0x1806
pub const SM_EXTERNAL_AUTH_REQUIRED_RSP: u16 = lmac_first_msg(TASK_SM) + 7; // 0x1807
pub const SM_FT_AUTH_IND: u16 = lmac_first_msg(TASK_SM) + 8; // 0x1808
pub const SM_FT_AUTH_RSP: u16 = lmac_first_msg(TASK_SM) + 9; // 0x1809
pub const SM_RSP_TIMEOUT_IND: u16 = lmac_first_msg(TASK_SM) + 10; // 0x180A
pub const SM_COEX_TS_TIMEOUT_IND: u16 = lmac_first_msg(TASK_SM) + 11; // 0x180B
pub const SM_EXTERNAL_AUTH_REQUIRED_RSP_CFM: u16 = lmac_first_msg(TASK_SM) + 12; // 0x180C

// ========== 连接 flags (mac_connection_flags) ==========
/// 控制端口由 host 管理（设置后 EAPOL 帧会透传给驱动）
pub const CONTROL_PORT_HOST: u32 = 1 << 0;
/// 控制端口帧不加密
pub const CONTROL_PORT_NO_ENC: u32 = 1 << 1;
/// 禁用 HT（WEP/TKIP 时需要）
pub const DISABLE_HT: u32 = 1 << 2;
/// 使用 WPA/WPA2 认证
pub const WPA_WPA2_IN_USE: u32 = 1 << 3;
/// 使用 MFP (802.11w)
pub const MFP_IN_USE: u32 = 1 << 4;
/// 重关联（roaming）
pub const REASSOCIATION: u32 = 1 << 5;

// ========== 认证类型 ==========
pub const WLAN_AUTH_OPEN: u8 = 0;
pub const WLAN_AUTH_SHARED_KEY: u8 = 1;
pub const WLAN_AUTH_FT: u8 = 2;
pub const WLAN_AUTH_SAE: u8 = 3;

// ========== Cipher suite (固件内部编号，非 IEEE OUI) ==========
pub const MAC_CIPHER_WEP40: u8 = 0;
pub const MAC_CIPHER_TKIP: u8 = 1;
pub const MAC_CIPHER_CCMP: u8 = 2;
pub const MAC_CIPHER_WEP104: u8 = 3;

// ========== mac_sec_key 常量 ==========
/// 密钥最大长度（字节）
pub const MAC_SEC_KEY_LEN: usize = 32;

// ========== 802.11 IE ID ==========
pub const WLAN_EID_SSID: u8 = 0;
pub const WLAN_EID_RSN: u8 = 48;

// ========== 802.1X EtherType ==========
// ETH_P_PAE 统一在 consts.rs 中定义，避免多重导出冲突

// ========== sm_connect_req 结构体布局常量 ==========
//
// 参考 Linux: struct sm_connect_req (lmac_msg.h:2406-2432)
//   struct mac_ssid ssid;           // 33 bytes (1 + 32)
//   struct mac_addr bssid;          // 6 bytes
//   struct mac_chan_def chan;        // 5 bytes
//   u32 flags;                      // 4 bytes
//   u16 ctrl_port_ethertype;        // 2 bytes
//   u16 ie_len;                     // 2 bytes
//   u16 listen_interval;            // 2 bytes
//   bool dont_wait_bcmc;            // 1 byte
//   u8 auth_type;                   // 1 byte
//   u8 uapsd_queues;               // 1 byte
//   u8 vif_idx;                     // 1 byte
//   u32 ie_buf[64];                // 256 bytes
//
// 注意：C 编译器会在字段间插入 padding 以满足对齐要求。
// 实际布局需要与固件匹配。以下偏移量基于 ARM/RISC-V 默认对齐规则：
//
//   offset 0:   ssid.length (u8)
//   offset 1:   ssid.array[32]
//   offset 33:  padding (1 byte, align bssid to u16)
//   offset 34:  bssid (6 bytes, u16[3])
//   offset 40:  chan.freq (u16) + chan.band (u8) + chan.flags (u8) + chan.tx_power (i8) = 5 bytes
//   offset 45:  padding (1 byte, align flags to u32)
//   offset 46:  更多 padding (2 bytes, align to u32)
//   ... 实际偏移取决于编译器
//
// 最安全的做法：使用 sizeof(struct sm_connect_req) 从 Linux 驱动获取确切大小。
// sm_connect_req 的 ie_buf 是 u32[64] = 256 bytes，总大小约 316 bytes。

/// sm_connect_req 的近似大小（不含 padding 的最小值）
/// 实际大小可能因 padding 而更大，建议先用此值尝试，
/// 如果固件拒绝则增加到 320 或 324
pub const SM_CONNECT_REQ_BASE_SIZE: usize = MAC_SSID_SIZE           // 33: ssid
    + MAC_ADDR_SIZE         // 6:  bssid
    + MAC_CHAN_DEF_SIZE     // 5:  chan
    + 4                     // flags (u32)
    + 2                     // ctrl_port_ethertype (u16)
    + 2                     // ie_len (u16)
    + 2                     // listen_interval (u16)
    + 1                     // dont_wait_bcmc (bool)
    + 1                     // auth_type (u8)
    + 1                     // uapsd_queues (u8)
    + 1                     // vif_idx (u8)
    + 256; // ie_buf (u32[64])
// = 33 + 6 + 5 + 4 + 2 + 2 + 2 + 1 + 1 + 1 + 1 + 256 = 314

/// sm_disconnect_req: reason_code(u16) + vif_idx(u8) = 3 bytes
pub const SM_DISCONNECT_REQ_SIZE: usize = 3;

/// mm_key_add_req 结构体布局:
///   u8 key_idx;                    // offset 0
///   u8 sta_idx;                    // offset 1
///   [2 bytes padding]             // offset 2-3 (align mac_sec_key.array to u32)
///   struct mac_sec_key key;        // offset 4: length(u8) + [3B pad] + array[u32;8](32B) = 36B
///   u8 cipher_suite;               // offset 40
///   u8 inst_nbr;                   // offset 41
///   u8 spp;                        // offset 42
///   bool pairwise;                 // offset 43
///   总计: 44 bytes
pub const MM_KEY_ADD_REQ_SIZE: usize = 44;

/// mm_key_del_req: hw_key_idx(u8) = 1 byte
pub const MM_KEY_DEL_REQ_SIZE: usize = 1;

/// me_set_control_port_req: sta_idx(u8) + control_port_open(bool) = 2 bytes
pub const ME_SET_CONTROL_PORT_REQ_SIZE: usize = 2;

/// sm_connect_ind 结构体布局（参考 lmac_msg.h:2444-2477）:
///   u16 status_code;
///   struct mac_addr bssid;         // 6 bytes
///   bool roamed;                   // 1 byte
///   u8 vif_idx;
///   u8 ap_idx;
///   u8 ch_idx;
///   bool qos;
///   u8 acm;
///   u16 assoc_req_ie_len;
///   u16 assoc_rsp_ie_len;
///   u32 assoc_ie_buf[200];         // SM_ASSOC_IE_LEN/4 = 800/4 = 200
///   u16 aid;
///   u8 band;
///   u16 center_freq;
///   u8 width;
///   u32 center_freq1;
///   u32 center_freq2;
///   u32 ac_param[4];               // AC_MAX = 4
pub const SM_ASSOC_IE_LEN: usize = 800;

/// sm_disconnect_ind: reason_code(u16) + vif_idx(u8) + ft_over_ds(bool) + reassoc(u8) = 5 bytes
pub const SM_DISCONNECT_IND_SIZE: usize = 5;

/// WiFi 连接状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiState {
    /// 未连接
    Disconnected,
    /// 正在扫描
    Scanning,
    /// 正在连接
    Connecting,
    /// 已连接（关联成功，但密钥可能尚未安装）
    Connected,
    /// 已认证（WPA2 握手完成，控制端口已打开）
    Authenticated,
}

/// 连接结果（从 SM_CONNECT_IND 解析）
#[derive(Clone, Debug)]
pub struct ConnectResult {
    pub status_code: u16,
    pub bssid: [u8; 6],
    pub ap_idx: u8,
    pub ch_idx: u8,
    pub vif_idx: u8,
    pub qos: bool,
    pub aid: u16,
    /// 固件实际发送的 Association Request IEs（从 SM_CONNECT_IND 的 assoc_ie_buf 提取）
    pub assoc_req_ies: Vec<u8>,
}

/// 断连信息（从 SM_DISCONNECT_IND 解析）
#[derive(Clone, Debug)]
pub struct DisconnectInfo {
    pub reason_code: u16,
    pub vif_idx: u8,
}
