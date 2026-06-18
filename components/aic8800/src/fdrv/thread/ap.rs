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

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{sync::atomic::Ordering, task::Poll};

use crate::fdrv::{
    consts::{CONTROL_PORT_MAX_RETRY, CONTROL_PORT_RECONCILE_MS, MAX_REGISTERED_STAS},
    core::bus::{BusState, WifiBus},
    protocol::{send_me_sta_add_req, send_mm_sta_del_req, send_set_control_port_req},
    thread::tx::enqueue_mgmt_frame,
};

/// 启动 AP worker 线程
pub fn start(bus: Arc<WifiBus>) {
    log::debug!("[wifi-ap] worker thread starting");
    crate::runtime::runtime().spawn_poll_task(
        "wifi-ap",
        Box::new(move |cx| {
            if *bus.state.lock() == BusState::Down {
                return Poll::Ready(());
            }

            // 先处理 STA 删除(deauth/disassoc 后释放固件 STA 槽位),再处理新关联,
            // 这样新 STA 能复用刚释放的低位 sta_idx(下行单一 sta_idx 路由只对低位
            // 槽位可靠)。
            loop {
                let del_idx = bus.ap.sta_del_queue.lock().pop_front();
                match del_idx {
                    Some(idx) => {
                        if let Err(e) = send_mm_sta_del_req(&bus, idx, 0) {
                            log::warn!("[wifi-ap] MM_STA_DEL sta_idx={} failed: {:?}", idx, e);
                        }
                    }
                    None => break,
                }
            }

            // 取出所有待处理的关联请求
            loop {
                let assoc_req = bus.ap.assoc_queue.lock().pop_front();
                match assoc_req {
                    Some(mpdu) => handle_assoc_req(&bus, &mpdu),
                    None => break,
                }
            }

            // 控制端口对账:对所有尚未授权的 STA 主动重试打开控制端口。不依赖手机
            // 重传 AssocReq——手机关联成功后通常不再发 AssocReq,若首次 authorize
            // 命令超时,数据面会永久不通(ping/SSH 不通)。这里兜底直到成功或到上限。
            let has_pending = reconcile_control_ports(&bus);

            bus.ap.assoc_pollset.register(cx.waker());
            // 注册后再检查一次，避免错过唤醒
            if !bus.ap.assoc_queue.lock().is_empty() {
                cx.waker().wake_by_ref();
            } else if has_pending {
                // 仍有未授权 STA:周期性自唤醒重试,直到全部授权或到重试上限。
                crate::runtime::runtime().sleep_ms(CONTROL_PORT_RECONCILE_MS);
                cx.waker().wake_by_ref();
            }
            Poll::Pending
        }),
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

    // 已注册的 STA(手机重传 AssocReq)：跳过 ME_STA_ADD,只补发 Assoc Response。
    // 固件对同一 MAC 的重复 ME_STA_ADD 不回 CFM,会让本 worker 阻塞 5 秒超时,
    // 期间连接抖动(实测 DHCP 退回 Discover)。重传多因手机没收到上一个 Assoc
    // Response,故补发即可。
    // 查注册表:返回 (sta_idx, 控制端口是否已开)。
    let existing = bus
        .ap
        .registered_stas
        .lock()
        .iter()
        .find(|(mac, ..)| *mac == sta_mac)
        .map(|(_, idx, ctrl, _)| (*idx, *ctrl));

    let (sta_idx, ctrl_open) = if let Some((idx, ctrl)) = existing {
        log::info!(
            "[wifi-ap] STA {:02x?} already registered (sta_idx={}, ctrl_open={}), resend Assoc \
             Response{}",
            sta_mac,
            idx,
            ctrl,
            if ctrl {
                " only"
            } else {
                " + retry control port"
            }
        );
        (idx, ctrl)
    } else {
        // 新 MAC:先检查注册表容量,避免异常情况下无界增长。
        if bus.ap.registered_stas.lock().len() >= MAX_REGISTERED_STAS {
            log::warn!(
                "[wifi-ap] registered_stas full ({}), reject new STA {:02x?}",
                MAX_REGISTERED_STAS,
                sta_mac
            );
            return;
        }
        // 注册 STA。固件对重复注册不回 CFM,故仅新 MAC 才发 ME_STA_ADD。
        let idx = match send_me_sta_add_req(bus, &sta_mac, &rates, aid, vif_idx, 0) {
            Ok(idx) => idx,
            Err(e) => {
                log::warn!("[wifi-ap] ME_STA_ADD failed: {:?}", e);
                return;
            }
        };
        bus.conn.sta_idx.store(idx, Ordering::Release);
        bus.ap.registered_stas.lock().push((sta_mac, idx, false, 0));
        log::info!(
            "[wifi-ap] STA {:02x?} registered: sta_idx={}, aid={}",
            sta_mac,
            idx,
            aid
        );
        (idx, false)
    };

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
    // 这里做首次尝试(低延迟);若超时,AP worker 的周期对账会继续重试直到成功。
    if !ctrl_open {
        try_open_control_port(bus, &sta_mac, sta_idx);
    }
}

/// 尝试为指定 STA 打开控制端口(authorize),成功则在注册表置标志。
/// 返回是否成功。失败(超时)不在此处重试——由调用方/周期对账驱动重试。
fn try_open_control_port(bus: &Arc<WifiBus>, sta_mac: &[u8; 6], sta_idx: u8) -> bool {
    match send_set_control_port_req(bus, sta_idx, true, 0) {
        Ok(_) => {
            log::info!("[wifi-ap] control port OPENED for sta_idx={}", sta_idx);
            if let Some(e) = bus
                .ap
                .registered_stas
                .lock()
                .iter_mut()
                .find(|(mac, ..)| mac == sta_mac)
            {
                e.2 = true;
            }
            true
        }
        Err(e) => {
            log::warn!(
                "[wifi-ap] open control port failed (sta_idx={}): {:?}",
                sta_idx,
                e
            );
            false
        }
    }
}

/// 周期对账:对注册表中所有"未授权且未到重试上限"的 STA 重试打开控制端口。
/// 返回是否仍有待授权的 STA(用于决定 AP worker 是否需要继续周期自唤醒)。
fn reconcile_control_ports(bus: &Arc<WifiBus>) -> bool {
    // 先快照出待重试的 (mac, idx),避免持锁期间发命令(send_cmd 会让出/阻塞)。
    let pending: alloc::vec::Vec<([u8; 6], u8)> = {
        let tbl = bus.ap.registered_stas.lock();
        tbl.iter()
            .filter(|(.., open, retries)| !*open && *retries < CONTROL_PORT_MAX_RETRY)
            .map(|(mac, idx, ..)| (*mac, *idx))
            .collect()
    };
    if pending.is_empty() {
        return false;
    }

    let mut still_pending = false;
    for (mac, idx) in pending {
        // 先自增重试计数(即便本次又超时,也能朝上限收敛,防止已离线 STA 永久空转)。
        {
            let mut tbl = bus.ap.registered_stas.lock();
            if let Some(e) = tbl.iter_mut().find(|(m, ..)| *m == mac) {
                e.3 = e.3.saturating_add(1);
            }
        }
        if !try_open_control_port(bus, &mac, idx) {
            still_pending = true;
        }
    }
    still_pending
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
