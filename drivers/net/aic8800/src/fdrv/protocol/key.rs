//! 密钥安装/删除命令

use alloc::{sync::Arc, vec::Vec};

use crate::fdrv::{
    core::bus::WifiBus,
    protocol::{cmd::send_cmd, lmac_msg::*},
};

/// 发送 MM_KEY_ADD_REQ
pub fn send_key_add_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    sta_idx: u8,
    pairwise: bool,
    key: &[u8],
    key_idx: u8,
    cipher_suite: u8,
    timeout_ms: u64,
) -> Result<u8, CmdError> {
    const MM_KEY_ADD_REQ_SIZE: usize = 44;
    let mut param = [0u8; MM_KEY_ADD_REQ_SIZE];

    param[0] = key_idx;
    param[1] = sta_idx;

    let key_len = key.len().min(MAC_SEC_KEY_LEN);
    param[4] = key_len as u8;
    param[8..8 + key_len].copy_from_slice(&key[..key_len]);

    param[40] = cipher_suite;
    param[41] = vif_idx;
    param[42] = 0;
    param[43] = if pairwise { 1 } else { 0 };

    let rsp = send_cmd(bus, MM_KEY_ADD_REQ, TASK_MM, &param, timeout_ms)?;

    if rsp.len() >= 2 {
        let status = rsp[0];
        let hw_key_idx = rsp[1];
        if status != 0 {
            log::error!("[cmd_mgr] MM_KEY_ADD_CFM status={} (error)", status);
            return Err(CmdError::FirmwareError);
        }
        Ok(hw_key_idx)
    } else {
        log::error!("[cmd_mgr] MM_KEY_ADD_CFM too short: {} bytes", rsp.len());
        Err(CmdError::InvalidResponse)
    }
}

/// 发送 MM_KEY_DEL_REQ
pub fn send_key_del_req(
    bus: &Arc<WifiBus>,
    hw_key_idx: u8,
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    let param = [hw_key_idx];
    send_cmd(bus, MM_KEY_DEL_REQ, TASK_MM, &param, timeout_ms)
}
