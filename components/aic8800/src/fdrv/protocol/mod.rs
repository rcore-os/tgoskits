//! 协议和命令模块

pub mod apm;
pub mod cmd;
pub mod config;
pub mod connection;
pub mod key;
pub mod lmac_msg;
pub mod scan;

// 重导出 cmd 中的公共 API（CMD 框架 + EAPOL）
pub use apm::{
    send_apm_set_beacon_ie_req, send_apm_start_req, send_apm_stop_req, send_me_sta_add_req,
    start_open_ap,
};
pub use cmd::*;
pub use config::{
    send_get_mac_addr_req, send_me_chan_config_req, send_me_config_req, send_me_set_ps_mode_req,
    send_mm_set_filter_req, send_mm_set_idle_req, send_mm_start_req, send_reset_req,
    send_rf_calib_req, send_set_control_port_req, send_set_stack_start_req, send_txpwr_idx_req,
    send_txpwr_ofst_req,
};
pub use connection::{
    send_mm_add_if_req, send_mm_add_if_req_typed, send_sm_connect_req, send_sm_disconnect_req,
    wait_for_indication,
};
pub use key::{send_key_add_req, send_key_del_req};
// 显式重导出 lmac_msg 中的协议定义
pub use lmac_msg::{
    APM_SET_BEACON_IE_CFM, APM_SET_BEACON_IE_REQ, APM_START_CAC_CFM, APM_START_CAC_REQ,
    APM_START_CFM, APM_START_REQ, APM_STOP_CAC_CFM, APM_STOP_CAC_REQ, APM_STOP_CFM, APM_STOP_REQ,
    CHAN_2G4_FREQS, CMD_TIMEOUT_MS, CMD_TX_TIMEOUT_DEFAULT_MS, CONTROL_PORT_HOST,
    CONTROL_PORT_NO_ENC, CmdError, ConnectResult, DISABLE_HT, DRV_TASK_ID, DisconnectInfo, LmacMsg,
    MAC_ADDR_LEN, MAC_ADDR_SIZE, MAC_CHAN_DEF_SIZE, MAC_CIPHER_CCMP, MAC_CIPHER_TKIP,
    MAC_CIPHER_WEP40, MAC_CIPHER_WEP104, MAC_SEC_KEY_LEN, MAC_SSID_LEN, MAC_SSID_SIZE,
    ME_CHAN_CONFIG_CFM, ME_CHAN_CONFIG_REQ, ME_CONFIG_CFM, ME_CONFIG_REQ, ME_SET_CONTROL_PORT_CFM,
    ME_SET_CONTROL_PORT_REQ, ME_SET_CONTROL_PORT_REQ_SIZE, MFP_IN_USE, MM_ADD_IF_CFM,
    MM_ADD_IF_REQ, MM_AP, MM_GET_MAC_ADDR_CFM, MM_GET_MAC_ADDR_REQ, MM_IBSS, MM_KEY_ADD_CFM,
    MM_KEY_ADD_REQ, MM_KEY_ADD_REQ_SIZE, MM_KEY_DEL_CFM, MM_KEY_DEL_REQ, MM_KEY_DEL_REQ_SIZE,
    MM_REMOVE_IF_CFM, MM_REMOVE_IF_REQ, MM_RESET_CFM, MM_RESET_REQ, MM_SET_CHANNEL_CFM,
    MM_SET_CHANNEL_REQ, MM_SET_FILTER_CFM, MM_SET_FILTER_REQ, MM_SET_IDLE_CFM, MM_SET_IDLE_REQ,
    MM_SET_RF_CALIB_CFM, MM_SET_RF_CALIB_REQ, MM_SET_RF_CONFIG_CFM, MM_SET_RF_CONFIG_REQ,
    MM_SET_STACK_START_CFM, MM_SET_STACK_START_REQ, MM_SET_TXPWR_IDX_LVL_CFM,
    MM_SET_TXPWR_IDX_LVL_REQ, MM_SET_TXPWR_OFST_CFM, MM_SET_TXPWR_OFST_REQ, MM_STA, MM_STA_ADD_CFM,
    MM_STA_ADD_REQ, MM_STA_DEL_CFM, MM_STA_DEL_REQ, MM_START_CFM, MM_START_REQ, MM_VERSION_CFM,
    MM_VERSION_REQ, PHY_CHNL_BW_20, PHY_CHNL_BW_40, PHY_CHNL_BW_80, REASSOCIATION,
    SCAN_CHANNEL_MAX, SCAN_SSID_MAX, SCANU_CANCEL_CFM, SCANU_CANCEL_REQ, SCANU_FAST_CFM,
    SCANU_FAST_REQ, SCANU_JOIN_CFM, SCANU_JOIN_REQ, SCANU_RESULT_IND, SCANU_START_CFM,
    SCANU_START_CFM_ADDTIONAL, SCANU_START_REQ, SCANU_START_REQ_SIZE, SCANU_VENDOR_IE_CFM,
    SCANU_VENDOR_IE_REQ, SM_ASSOC_IE_LEN, SM_COEX_TS_TIMEOUT_IND, SM_CONNECT_CFM, SM_CONNECT_IND,
    SM_CONNECT_REQ, SM_CONNECT_REQ_BASE_SIZE, SM_DISCONNECT_CFM, SM_DISCONNECT_IND,
    SM_DISCONNECT_IND_SIZE, SM_DISCONNECT_REQ, SM_DISCONNECT_REQ_SIZE,
    SM_EXTERNAL_AUTH_REQUIRED_IND, SM_EXTERNAL_AUTH_REQUIRED_RSP,
    SM_EXTERNAL_AUTH_REQUIRED_RSP_CFM, SM_FT_AUTH_IND, SM_FT_AUTH_RSP, SM_RSP_TIMEOUT_IND,
    ScanResult, TASK_API, TASK_APM, TASK_BAM, TASK_DBG, TASK_ME, TASK_MESH, TASK_MM, TASK_RM,
    TASK_RXU, TASK_SCAN, TASK_SCANU, TASK_SM, TASK_TDLS, TASK_TWT, WLAN_AUTH_FT, WLAN_AUTH_OPEN,
    WLAN_AUTH_SAE, WLAN_AUTH_SHARED_KEY, WLAN_EID_RSN, WLAN_EID_SSID, WPA_WPA2_IN_USE, WifiState,
    lmac_first_msg, msg_index, msg_task,
};
// 重导出子模块公共 API
pub use scan::{collect_scan_results, send_scanu_start_req};
