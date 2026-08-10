//! WiFi 扫描命令和结果收集

use alloc::{sync::Arc, vec, vec::Vec};

use crate::fdrv::{
    core::bus::WifiBus,
    protocol::{
        cmd::{current_time_ms, send_cmd_with_cfm_id},
        lmac_msg::*,
    },
};

/// 发送 SCANU_START_REQ
pub fn send_scanu_start_req(
    bus: &Arc<WifiBus>,
    vif_idx: u8,
    ssid: Option<&[u8]>,
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    let mut param = vec![0u8; SCANU_START_REQ_SIZE];

    // 填充 chan[0..14]（2.4GHz 信道 1-14）
    let chan_cnt = CHAN_2G4_FREQS.len().min(SCAN_CHANNEL_MAX);
    for i in 0..chan_cnt {
        let off = i * MAC_CHAN_DEF_SIZE;
        param[off..off + 2].copy_from_slice(&CHAN_2G4_FREQS[i].to_le_bytes());
        param[off + 2] = 0; // band
        param[off + 3] = 0; // flags
        param[off + 4] = 30; // tx_power
    }

    // 填充 ssid[0]
    let ssid_offset = SCAN_CHANNEL_MAX * MAC_CHAN_DEF_SIZE;
    let ssid_cnt = if let Some(s) = ssid {
        let len = s.len().min(MAC_SSID_LEN);
        param[ssid_offset] = len as u8;
        param[ssid_offset + 1..ssid_offset + 1 + len].copy_from_slice(&s[..len]);
        1u8
    } else {
        0u8
    };

    // 填充 bssid（广播地址）
    let bssid_offset = ssid_offset + SCAN_SSID_MAX * MAC_SSID_SIZE + 1;
    param[bssid_offset..bssid_offset + 6].copy_from_slice(&[0xFF; 6]);

    // 尾部字段
    let tail_offset = bssid_offset + MAC_ADDR_SIZE + 2;
    param[tail_offset + 6] = vif_idx;
    param[tail_offset + 7] = chan_cnt as u8;
    param[tail_offset + 8] = ssid_cnt;
    param[tail_offset + 9] = 0;

    log::debug!(
        "[cmd_mgr] sending SCANU_START_REQ: vif_idx={}, chan_cnt={}, ssid_cnt={}, param_size={}",
        vif_idx,
        chan_cnt,
        ssid_cnt,
        param.len()
    );

    send_cmd_with_cfm_id(
        bus,
        SCANU_START_REQ,
        TASK_SCANU,
        &param,
        SCANU_START_CFM_ADDTIONAL,
        timeout_ms,
    )
}

// ===== 扫描结果收集 =====

fn is_scan_related_message(msg_id: u16) -> bool {
    msg_id == SCANU_RESULT_IND || msg_id == SCANU_START_CFM || msg_id == SCANU_START_CFM_ADDTIONAL
}

fn is_interesting_indication(msg_id: u16) -> bool {
    msg_id != MM_CHANNEL_SURVEY_IND
}

fn find_scan_message_in_queue(bus: &Arc<WifiBus>) -> Option<(Vec<u8>, u16)> {
    let mut queue = bus.tx.ind_queue.lock();
    for i in 0..queue.len() {
        if queue[i].len() < LmacMsg::SIZE {
            continue;
        }
        let msg = LmacMsg::from_le_bytes(&queue[i]);
        if !is_interesting_indication(msg.id) {
            continue;
        }
        if is_scan_related_message(msg.id) {
            let msg_data = queue.remove(i).unwrap();
            return Some((msg_data, msg.id));
        }
    }
    None
}

fn process_scan_message(msg_data: &[u8], _msg_id: u16) -> (bool, Option<ScanResult>) {
    let msg = LmacMsg::from_le_bytes(msg_data);

    if msg.id == SCANU_START_CFM {
        log::debug!("[collect] SCANU_START_CFM received, scan truly complete");
        return (false, None);
    }

    if msg.id == SCANU_START_CFM_ADDTIONAL {
        log::debug!("[collect] SCANU_START_CFM_ADDTIONAL in ind_queue, skipping");
        return (true, None);
    }

    let param = &msg_data[LmacMsg::SIZE..];
    if let Some(result) = parse_scanu_result_ind(param) {
        (true, Some(result))
    } else {
        (true, None)
    }
}

fn merge_scan_result_by_rssi(existing: &mut ScanResult, new: &ScanResult) {
    let new_freq = new.center_freq;
    let new_rssi = new.rssi;

    if new_rssi > existing.rssi {
        let old_freq = existing.center_freq;
        let old_rsn = if existing.rsn_ie.is_empty() {
            existing.rsn_ie.clone()
        } else {
            Vec::new()
        };

        *existing = new.clone();

        if existing.center_freq == 0 && old_freq != 0 {
            existing.center_freq = old_freq;
        }
        if existing.rsn_ie.is_empty() && !old_rsn.is_empty() {
            existing.rsn_ie = old_rsn;
        }
    } else {
        if existing.center_freq == 0 && new_freq != 0 {
            existing.center_freq = new_freq;
        }
        if existing.rsn_ie.is_empty() && !new.rsn_ie.is_empty() {
            existing.rsn_ie = new.rsn_ie.clone();
        }
    }
}

fn dedup_scan_results(results: Vec<ScanResult>) -> Vec<ScanResult> {
    let before_dedup = results.len();
    let mut deduped: Vec<ScanResult> = Vec::new();

    for r in results {
        if let Some(existing) = deduped.iter_mut().find(|e| e.bssid == r.bssid) {
            merge_scan_result_by_rssi(existing, &r);
        } else {
            deduped.push(r);
        }
    }

    if before_dedup != deduped.len() {
        log::debug!(
            "[collect] deduplicated: {} -> {} unique APs",
            before_dedup,
            deduped.len()
        );
    }

    deduped
}

/// 从 ind_queue 中收集所有 SCANU_RESULT_IND 并解析为 ScanResult
pub fn collect_scan_results(bus: &Arc<WifiBus>, timeout_ms: u64) -> Vec<ScanResult> {
    let mut results = Vec::new();
    let deadline = current_time_ms() + timeout_ms;

    loop {
        let now = current_time_ms();
        if now >= deadline {
            log::warn!("[collect] scan collection timed out after {}ms", timeout_ms);
            break;
        }

        match find_scan_message_in_queue(bus) {
            Some((msg_data, msg_id)) => {
                let (should_continue, scan_result) = process_scan_message(&msg_data, msg_id);
                if !should_continue {
                    break;
                }
                if let Some(result) = scan_result {
                    results.push(result);
                }
            }
            None => {
                crate::runtime::runtime().yield_now();
            }
        }
    }

    let deduped = dedup_scan_results(results);
    log::debug!("[collect] total {} APs found", deduped.len());
    deduped
}

fn channel_to_freq(channel: u8) -> u16 {
    match channel {
        1..=13 => 2407 + (channel as u16) * 5,
        14 => 2484,
        36..=177 => 5000 + (channel as u16) * 5,
        _ => 0,
    }
}

/// 解析 SCANU_RESULT_IND 的 param 部分
fn parse_scanu_result_ind(param: &[u8]) -> Option<ScanResult> {
    if param.len() < 12 {
        log::warn!("[parse] SCANU_RESULT_IND too short: {} bytes", param.len());
        return None;
    }

    let _length = u16::from_le_bytes([param[0], param[1]]);
    let _framectrl = u16::from_le_bytes([param[2], param[3]]);
    let mut center_freq = u16::from_le_bytes([param[4], param[5]]);
    let _band = param[6];
    let _sta_idx = param[7];
    let _inst_nbr = param[8];
    let rssi = param[9] as i8;
    let payload = &param[12..];

    if payload.len() < 36 {
        log::warn!(
            "[parse] payload too short for 802.11 header: {}",
            payload.len()
        );
        return None;
    }

    let mut bssid = [0u8; 6];
    bssid.copy_from_slice(&payload[16..22]);

    let beacon_interval = u16::from_le_bytes([payload[32], payload[33]]);
    let capability = u16::from_le_bytes([payload[34], payload[35]]);

    // 解析 IE
    let ie_data = &payload[36..];
    let mut ssid = [0u8; MAC_SSID_LEN];
    let mut ssid_len: u8 = 0;
    let mut ds_channel: u8 = 0;
    let mut rsn_ie = Vec::new();

    let mut ie_offset = 0;
    while ie_offset + 2 <= ie_data.len() {
        let ie_id = ie_data[ie_offset];
        let ie_len = ie_data[ie_offset + 1] as usize;

        if ie_offset + 2 + ie_len > ie_data.len() {
            break;
        }

        match ie_id {
            0 => {
                ssid_len = ie_len.min(MAC_SSID_LEN) as u8;
                ssid[..ssid_len as usize]
                    .copy_from_slice(&ie_data[ie_offset + 2..ie_offset + 2 + ssid_len as usize]);
            }
            3 if ie_len >= 1 => {
                ds_channel = ie_data[ie_offset + 2];
            }
            0x30 => {
                rsn_ie = ie_data[ie_offset..ie_offset + 2 + ie_len].to_vec();
            }
            _ => {}
        }
        ie_offset += 2 + ie_len;
    }

    if center_freq == 0 && ds_channel != 0 {
        center_freq = channel_to_freq(ds_channel);
    }

    log::debug!(
        "[parse] AP: ssid=\"{}\", bssid={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, freq={}, \
         rssi={}",
        core::str::from_utf8(&ssid[..ssid_len as usize]).unwrap_or("<invalid>"),
        bssid[0],
        bssid[1],
        bssid[2],
        bssid[3],
        bssid[4],
        bssid[5],
        center_freq,
        rssi
    );

    Some(ScanResult {
        ssid,
        ssid_len,
        bssid,
        center_freq,
        rssi,
        capability,
        beacon_interval,
        raw_payload: payload.to_vec(),
        rsn_ie,
    })
}
