//! AP worker 线程
//!
//! 处理 AP 模式下的关联流程。RX 线程收到 AssocReq 后把整帧入队
//! `bus.ap.assoc_queue`，本线程取出后：
//!   1. 解析 SupportedRates
//!   2. ME_STA_ADD_REQ 注册 STA，拿固件分配的 sta_idx
//!   3. 构造并发送 Assoc Response (status=0, 带 AID)
//!
//! 必须独立于 RX 线程：ME_STA_ADD 走 send_cmd 阻塞等 CFM，而 CFM 由
//! RX 线程接收处理 —— 在 RX 线程里调 send_cmd 会死锁。

use alloc::{sync::Arc, vec::Vec};
use core::{future::poll_fn, sync::atomic::Ordering, task::Poll};

use ax_task::future::block_on;

use crate::fdrv::{
    core::bus::{BusState, WifiBus},
    protocol::{send_me_sta_add_req, send_set_control_port_req},
    thread::tx::enqueue_mgmt_frame,
};

/// 启动 AP worker 线程
pub fn start(bus: Arc<WifiBus>) {
    ax_task::spawn_with_name(
        move || {
            log::debug!("[wifi-ap] worker thread started");
            block_on(poll_fn(|cx| {
                if *bus.state.lock() == BusState::Down {
                    return Poll::Ready(());
                }

                // 取出所有待处理的关联请求
                loop {
                    let assoc_req = bus.ap.assoc_queue.lock().pop_front();
                    match assoc_req {
                        Some(mpdu) => handle_assoc_req(&bus, &mpdu),
                        None => break,
                    }
                }

                bus.ap.assoc_pollset.register(cx.waker());
                // 注册后再检查一次，避免错过唤醒
                if !bus.ap.assoc_queue.lock().is_empty() {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }));
            log::debug!("[wifi-ap] worker thread exited");
        },
        "wifi-ap".into(),
    );
}

/// 处理一个关联请求：注册 STA + 回 Assoc Response。
fn handle_assoc_req(bus: &Arc<WifiBus>, mpdu: &[u8]) {
    // 管理帧地址：addr2=SA@10 是手机 MAC
    if mpdu.len() < 28 {
        return;
    }
    let mut sta_mac = [0u8; 6];
    sta_mac.copy_from_slice(&mpdu[10..16]);

    let vif_idx = bus.conn.vif_idx.load(Ordering::Acquire);
    let aid: u16 = 1;

    // AssocReq body: mgmt hdr(24) + cap(2) + listen_int(2) + IEs
    let rates = parse_supported_rates(&mpdu[28..]);

    // 1. 注册 STA
    let sta_idx = match send_me_sta_add_req(bus, &sta_mac, &rates, aid, vif_idx, 0) {
        Ok(idx) => idx,
        Err(e) => {
            log::warn!("[wifi-ap] ME_STA_ADD failed: {:?}", e);
            return;
        }
    };
    bus.conn.sta_idx.store(sta_idx, Ordering::Release);
    log::info!(
        "[wifi-ap] STA {:02x?} registered: sta_idx={}, aid={}",
        sta_mac,
        sta_idx,
        aid
    );

    // 2. 回 Assoc Response
    let ap_mac = match *bus.conn.sta_mac.lock() {
        Some(m) => m,
        None => {
            log::warn!("[wifi-ap] no AP mac, cannot send Assoc Response");
            return;
        }
    };
    let frame = build_assoc_response(&sta_mac, &ap_mac, aid, &rates);
    match enqueue_mgmt_frame(bus, frame) {
        Ok(()) => log::info!("[wifi-ap] Assoc Response queued -> {:02x?}", sta_mac),
        Err(e) => log::warn!("[wifi-ap] Assoc Response enqueue failed: {:?}", e),
    }

    // 3. 打开控制端口(authorize)。开放网络无 EAPOL，关联后必须显式授权，
    // 否则固件只放行 EAPOL、丢弃该 STA 的所有普通数据帧(DHCP/ARP/IP)。
    // 对应 vendor change_station(AUTHORIZED) → rwnx_send_me_set_control_port_req。
    match send_set_control_port_req(bus, sta_idx, true, 0) {
        Ok(_) => log::info!("[wifi-ap] control port OPENED for sta_idx={}", sta_idx),
        Err(e) => log::warn!("[wifi-ap] open control port failed: {:?}", e),
    }
}

/// 从关联请求的 IE 区解析 SupportedRates (EID=1)，返回原始速率字节。
fn parse_supported_rates(ies: &[u8]) -> Vec<u8> {
    let mut i = 0;
    while i + 2 <= ies.len() {
        let eid = ies[i];
        let len = ies[i + 1] as usize;
        if i + 2 + len > ies.len() {
            break;
        }
        if eid == 1 {
            // SupportedRates
            return ies[i + 2..i + 2 + len].to_vec();
        }
        i += 2 + len;
    }
    // 兜底：802.11b/g 基础速率 (1,2,5.5,11 Mbps，带 basic 位)
    Vec::from([0x82, 0x84, 0x8b, 0x96])
}

/// 构造开放网络 Assoc Response 帧。
///
/// 布局：mgmt 头(24) + cap(2) + status(2) + AID(2) + SupportedRates IE。
fn build_assoc_response(dst: &[u8; 6], ap_mac: &[u8; 6], aid: u16, rates: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(40);
    f.extend_from_slice(&[0x10, 0x00]); // fc: mgmt, subtype=AssocResp(0x1)
    f.extend_from_slice(&[0x00, 0x00]); // duration
    f.extend_from_slice(dst); // addr1 = DA (手机)
    f.extend_from_slice(ap_mac); // addr2 = SA (AP)
    f.extend_from_slice(ap_mac); // addr3 = BSSID
    f.extend_from_slice(&[0x00, 0x00]); // seq ctrl (固件填)

    // capability info：ESS + short preamble (与 beacon 一致)
    f.extend_from_slice(&0x0021u16.to_le_bytes());
    // status code = success(0)
    f.extend_from_slice(&0u16.to_le_bytes());
    // AID：高 2 位置 1 (IEEE 规定)
    f.extend_from_slice(&(aid | 0xC000).to_le_bytes());

    // SupportedRates IE：EID=1, len, rates
    let n = rates.len().min(8);
    f.push(1);
    f.push(n as u8);
    f.extend_from_slice(&rates[..n]);

    f
}
