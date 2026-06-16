//! WiFi 连接/断连命令和 indication 等待

use alloc::{sync::Arc, vec, vec::Vec};
use core::task::Poll;

use crate::{
    fdrv::{
        consts::ETH_P_PAE,
        core::bus::WifiBus,
        protocol::{cmd::send_cmd, lmac_msg::*},
    },
    runtime::runtime,
};

// ===== Indication 等待 =====

fn check_timeout_and_log_queue(bus: &WifiBus, target_msg_id: u16, deadline: u64) -> bool {
    if crate::runtime::runtime().now_nanos() >= deadline {
        let queue = bus.tx.ind_queue.lock();
        log::error!(
            "[wait_ind] TIMEOUT waiting for msg_id=0x{:04x}, ind_queue has {} messages:",
            target_msg_id,
            queue.len()
        );
        for (i, msg_data) in queue.iter().enumerate() {
            if msg_data.len() >= LmacMsg::SIZE {
                let msg = LmacMsg::from_le_bytes(msg_data);
                log::error!(
                    "[wait_ind]   [{}] msg_id=0x{:04x}, param_len={}",
                    i,
                    msg.id,
                    msg.param_len
                );
            } else {
                log::error!(
                    "[wait_ind]   [{}] raw_len={} (too short)",
                    i,
                    msg_data.len()
                );
            }
        }
        return true;
    }
    false
}

fn extract_message_param(raw: &[u8]) -> Vec<u8> {
    let param_start = LmacMsg::SIZE;
    if raw.len() > param_start {
        raw[param_start..].to_vec()
    } else {
        Vec::new()
    }
}

fn extract_message_param_ref(raw: &[u8]) -> &[u8] {
    let param_start = LmacMsg::SIZE;
    if raw.len() > param_start {
        &raw[param_start..]
    } else {
        &[]
    }
}

fn handle_abort_message(msg_id: u16, param: &[u8]) {
    match msg_id {
        SM_DISCONNECT_IND => {
            let reason = if param.len() >= 4 {
                u16::from_le_bytes([param[2], param[3]])
            } else {
                0xFFFF
            };
            log::error!(
                "[wait_ind] SM_DISCONNECT_IND received! reason_code={}, param={:02x?}",
                reason,
                &param[..param.len().min(16)]
            );
        }
        SM_EXTERNAL_AUTH_REQUIRED_IND => {
            log::error!(
                "[wait_ind] SM_EXTERNAL_AUTH_REQUIRED_IND received! param={:02x?}",
                &param[..param.len().min(48)]
            );
        }
        _ => {
            log::error!(
                "[wait_ind] abort msg_id=0x{:04x} received, param={:02x?}",
                msg_id,
                &param[..param.len().min(16)]
            );
        }
    }
}

fn try_find_message_in_queue(
    bus: &WifiBus,
    target_msg_id: u16,
    abort_ids: &[u16],
) -> Option<Result<Vec<u8>, CmdError>> {
    let mut queue = bus.tx.ind_queue.lock();
    for i in 0..queue.len() {
        if queue[i].len() < LmacMsg::SIZE {
            continue;
        }
        let msg = LmacMsg::from_le_bytes(&queue[i]);

        if msg.id == target_msg_id {
            let raw = queue.remove(i).unwrap();
            let param = extract_message_param(&raw);
            return Some(Ok(param));
        }

        if abort_ids.contains(&msg.id) {
            let raw = queue.remove(i).unwrap();
            let param = extract_message_param_ref(&raw);
            handle_abort_message(msg.id, param);
            return Some(Err(CmdError::FirmwareError));
        }
    }
    None
}

/// 从 ind_queue 中等待指定 msg_id 的 indication
pub fn wait_for_indication(
    bus: &Arc<WifiBus>,
    target_msg_id: u16,
    abort_msg_ids: &[u16],
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    let deadline = runtime().now_nanos() + timeout_ms * 1_000_000;

    let mut out: Option<Result<Vec<u8>, CmdError>> = None;
    // 超时由 poll 体内部的 deadline 判定，故 block_until 不另设超时。
    let _ = runtime().block_until(None, &mut |cx| {
        if check_timeout_and_log_queue(bus, target_msg_id, deadline) {
            out = Some(Err(CmdError::Timeout));
            return Poll::Ready(());
        }

        if let Some(result) = try_find_message_in_queue(bus, target_msg_id, abort_msg_ids) {
            out = Some(result);
            return Poll::Ready(());
        }

        bus.tx.ind_pollset.register(cx.waker());

        if let Some(result) = try_find_message_in_queue(bus, target_msg_id, abort_msg_ids) {
            out = Some(result);
            return Poll::Ready(());
        }

        cx.waker().wake_by_ref();
        Poll::Pending
    });
    out.unwrap_or(Err(CmdError::Timeout))
}

// ===== 连接/断连命令 =====

/// 发送 SM_CONNECT_REQ
pub fn send_sm_connect_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    ssid: &[u8],
    bssid: &[u8; 6],
    channel_freq: u16,
    flags: u32,
    auth_type: u8,
    ie: &[u8],
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    const SM_CONNECT_REQ_SIZE: usize = 320;

    let mut param = vec![0u8; SM_CONNECT_REQ_SIZE];

    let ssid_len = ssid.len().min(MAC_SSID_LEN);
    param[0] = ssid_len as u8;
    param[1..1 + ssid_len].copy_from_slice(&ssid[..ssid_len]);

    param[34..40].copy_from_slice(bssid);

    if channel_freq != 0 && channel_freq != 0xFFFF {
        param[40..42].copy_from_slice(&channel_freq.to_le_bytes());
        param[42] = 0;
        param[43] = 0;
        param[44] = 30;
    } else {
        param[40..42].copy_from_slice(&0xFFFFu16.to_le_bytes());
    }

    param[48..52].copy_from_slice(&flags.to_le_bytes());
    param[52..54].copy_from_slice(&ETH_P_PAE.to_be_bytes());

    let ie_len = ie.len().min(256);
    param[54..56].copy_from_slice(&(ie_len as u16).to_le_bytes());
    param[56..58].copy_from_slice(&1u16.to_le_bytes());
    param[58] = 0;
    param[59] = auth_type;
    param[60] = 0;
    param[61] = vif_idx;

    if ie_len > 0 {
        param[64..64 + ie_len].copy_from_slice(&ie[..ie_len]);
    }

    send_cmd(bus, SM_CONNECT_REQ, TASK_SM, &param, timeout_ms)
}

/// 发送 SM_DISCONNECT_REQ
pub fn send_sm_disconnect_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    reason_code: u16,
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    let mut param = [0u8; 3];
    param[0..2].copy_from_slice(&reason_code.to_le_bytes());
    param[2] = vif_idx;

    send_cmd(bus, SM_DISCONNECT_REQ, TASK_SM, &param, timeout_ms)
}

/// 发送 MM_ADD_IF_REQ
/// 对应 Linux: struct mm_add_if_req (lmac_msg.h:642-649)
///   u8 type;           // offset 0: MM_STA=0
///   [1 byte padding]   // offset 1: mac_addr 2字节对齐
///   mac_addr addr;     // offset 2: 6 bytes
///   bool p2p;          // offset 8: false
///   [1 byte padding]   // offset 9: struct 2字节对齐
///   Total: 10 bytes
pub fn send_mm_add_if_req(
    bus: &Arc<WifiBus>,
    mac_addr: &[u8; 6],
    timeout_ms: u64,
) -> Result<u8, CmdError> {
    send_mm_add_if_req_typed(bus, mac_addr, MM_STA, timeout_ms)
}

/// 发送 MM_ADD_IF_REQ（指定接口类型）
///
/// `if_type`: MM_STA(0) / MM_IBSS(1) / MM_AP(2)
pub fn send_mm_add_if_req_typed(
    bus: &Arc<WifiBus>,
    mac_addr: &[u8; 6],
    if_type: u8,
    timeout_ms: u64,
) -> Result<u8, CmdError> {
    let mut param = [0u8; MM_ADD_IF_REQ_SIZE];
    param[0] = if_type; // type at offset 0
    // param[1] = 0 (padding for mac_addr alignment)
    param[2..8].copy_from_slice(mac_addr); // addr at offset 2
    param[8] = 0; // p2p = false

    let rsp = send_cmd(bus, MM_ADD_IF_REQ, TASK_MM, &param, timeout_ms)?;

    if rsp.len() >= 2 {
        let status = rsp[0];
        let vif_idx = rsp[1];
        if status != 0 {
            log::error!("[cmd_mgr] MM_ADD_IF_CFM status={} (error)", status);
            return Err(CmdError::FirmwareError);
        }
        Ok(vif_idx)
    } else {
        Err(CmdError::InvalidResponse)
    }
}

/// 发送 MM_REMOVE_IF_REQ —— 释放固件侧的虚拟接口（VIF）。
///
/// 对应 Linux: struct mm_remove_if_req { u8_l inst_nbr; }（仅 1 字节，即 vif_idx）。
/// MM_REMOVE_IF_CFM 无 payload。
///
/// 模式切换（STA↔AP）前必须调用，否则旧 VIF 在固件中永不释放，
/// `vif_idx` 被新接口覆盖后会造成 VIF 泄漏与寻址错乱。
pub fn send_mm_remove_if_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    timeout_ms: u64,
) -> Result<(), CmdError> {
    let param = [vif_idx];
    send_cmd(bus, MM_REMOVE_IF_REQ, TASK_MM, &param, timeout_ms)?;
    log::debug!("[cmd_mgr] MM_REMOVE_IF done (vif_idx={})", vif_idx);
    Ok(())
}
