use alloc::{sync::Arc, vec, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    task::Poll,
};

use log;

use crate::{
    common::{SDIO_TYPE_CFG, SDIO_TYPE_CFG_CMD_RSP, SDIO_TYPE_CFG_DATA_CFM, SDIO_TYPE_CFG_PRINT},
    fdrv::{
        consts::{
            BLOCK_COUNT_MASK, ETH_P_PAE, MAX_PKT_LEN, RX_ALIGNMENT, RX_HWHRD_LEN,
            SDIO_OTHER_INTERRUPT, SDIOWIFI_FUNC_BLOCKSIZE,
        },
        core::bus::{BusState, WifiBus},
    },
};

pub static RX_WAKE_COUNT: AtomicU64 = AtomicU64::new(0);

/// DIAG: kicker 唤醒次数,用于确诊 kicker 是否真在跑。
static RX_KICK_COUNT: AtomicU64 = AtomicU64::new(0);

/// 上层(StarryOS)注册的"收到数据帧"回调,存为 `fn()` 裸指针。
///
/// AIC8800 是 SDIO WiFi,RX 走自己的线程并独占 SDIO CARD_INT (IRQ#38),
/// 不经过 ax_net 的以太网 IRQ 框架。因此数据帧入队后,需主动通知网络栈
/// 来驱动一轮 poll(否则进来的 ARP/ICMP/数据包无人处理)。上层把此回调
/// 设为 `ax_net::poll_interfaces`,反转依赖,避免本 crate 直接依赖网络栈。
static RX_DATA_CALLBACK: AtomicUsize = AtomicUsize::new(0);

/// 本批 RX 是否有数据帧入队(由 `build_and_enqueue_eth_frame` 置位,
/// RX 线程处理完一批后读取并清除,据此决定是否驱动网络栈 poll)。
static RX_DATA_PENDING: AtomicBool = AtomicBool::new(false);

/// 注册"收到数据帧"回调(由 StarryOS 在注册 wlan0 后调用)。
pub fn register_rx_data_callback(cb: fn()) {
    RX_DATA_CALLBACK.store(cb as usize, Ordering::Release);
}

/// 若本批有数据帧入队且已注册回调,则调用回调驱动网络栈 poll。
fn invoke_rx_data_callback() {
    if !RX_DATA_PENDING.swap(false, Ordering::AcqRel) {
        return;
    }
    let ptr = RX_DATA_CALLBACK.load(Ordering::Acquire);
    if ptr != 0 {
        // SAFETY: ptr 来自 register_rx_data_callback 存入的 `fn()`。
        let cb: fn() = unsafe { core::mem::transmute(ptr) };
        cb();
    }
}

fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

/// 启动 wifi-rx 线程
pub fn start(bus: Arc<WifiBus>) {
    log::debug!("[wifi-rx] thread starting");
    start_rx_poll_kicker(bus.clone());
    crate::runtime::runtime().spawn_poll_task(
        "wifi-rx",
        alloc::boxed::Box::new(move |cx| {
            // 检查总线状态
            if *bus.state.lock() == BusState::Down {
                return Poll::Ready(());
            }

            // 检查并清除 ISR 标志
            if bus.rx.irq_pending.swap(false, Ordering::AcqRel) {
                RX_WAKE_COUNT.fetch_add(1, Ordering::Relaxed);
            }

            // 处理所有待读数据（内部会 mask CARD_INT，但不 unmask）
            process_rx_frames(&bus);

            // 先注册 waker，再 unmask CARD_INT
            // 这样 ISR 触发时 waker 已经就位，不会丢失唤醒
            bus.rx.irq_waker.register(cx.waker());

            // 关键：先 register waker，再 unmask CARD_INT
            // 如果 ISR 在 unmask 后立即触发，waker 已经注册好了
            bus.transport.unmask_card_irq();

            // 若本批有数据帧入队,驱动网络栈处理(AP/STA 收包)。
            invoke_rx_data_callback();

            // 双重检查：如果 ISR 在 register 和 unmask 之间触发了
            if bus.rx.irq_pending.swap(false, Ordering::AcqRel) {
                process_rx_frames(&bus);
                bus.transport.unmask_card_irq();
                invoke_rx_data_callback();
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            Poll::Pending
        }),
    );
}

/// 周期性唤醒 RX 线程去轮询 func2 的兜底任务。
///
/// 背景:RX 线程纯靠 waker 驱动,唤醒源只有 ISR(IRQ#38)和每次 TX。命令 CFM
/// 因发命令时顺带 wake 而能收到;但异步到来的帧(如 STA WPA2 握手的 EAPOL M1)
/// 没有 TX 触发,若 IRQ#38 不可靠,RX 线程会一直睡、帧烂在 func2 FIFO 没人读。
/// 这里每 10ms 唤醒一次 RX,确保异步入站帧能被及时捞出。
fn start_rx_poll_kicker(bus: Arc<WifiBus>) {
    crate::runtime::runtime().spawn_poll_task(
        "wifi-rx-kick",
        alloc::boxed::Box::new(move |cx| {
            if *bus.state.lock() == BusState::Down {
                return Poll::Ready(());
            }
            // 唤醒 RX 线程去 poll func2(不自己做 SDIO,避免与 RX 线程争锁)
            bus.rx.irq_waker.wake();
            // 每 ~1s 打一次心跳:证明 kicker 活着 + 汇报 RX 看到的 func2 状态
            let kicks = RX_KICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if kicks.is_multiple_of(100) {
                log::trace!("[RXKICK] alive kicks={}", kicks);
            }
            // 阻塞 sleep:本任务独占一个 ax_task 线程,只拖慢自己
            crate::runtime::runtime().sleep_ms(10);
            // 自唤醒,让 block_on 立即重新 poll,形成 10ms 周期循环
            cx.waker().wake_by_ref();
            Poll::Pending
        }),
    );
}

// ===== RX 帧读取处理 =====

/// 读取 block_cnt 并处理 SDIO_OTHER_INTERRUPT 重试
///
/// # 返回
/// (block_cnt, should_continue) - 读取的块计数和是否应该继续处理
fn read_block_count_with_retry(bus: &WifiBus, func: u8, other_int_retries: &mut u32) -> (u8, bool) {
    let intstatus = {
        match bus.transport.read_byte(func, bus.transport.block_cnt_reg()) {
            Ok(v) => v,
            Err(e) => {
                log::error!("[wifi-rx] read block_cnt(func{}) failed: {:?}", func, e);
                return (0, false);
            }
        }
    };

    if intstatus & SDIO_OTHER_INTERRUPT != 0 {
        *other_int_retries += 1;
        if *other_int_retries > 3 {
            log::trace!(
                "[wifi-rx] SDIO_OTHER_INTERRUPT persists after {} retries, giving up",
                other_int_retries
            );
            return (0, false);
        }
        log::trace!(
            "[wifi-rx] SDIO_OTHER_INTERRUPT (0x{:02x}), re-read",
            intstatus
        );
        return (0, true); // 继续重试
    }

    // V3(D80)下 0x04 是多功能 MISC_INT_STATUS 寄存器:bit7=SDIO_OTHER_INTERRUPT
    // (上面已处理),其余位的语义不是简单的"块数",而是 func1/func2 队列 + byte/block
    // 模式的复合编码(详见 resolve_rx_data_len)。这里只剥掉中断位,把原始 intstatus
    // 交给 resolve_rx_data_len 按厂商逻辑解析,不能在此直接 `& 0x7F` 当块数——例如
    // intstatus==120(0x78)是 func1 的 byte-mode 哨兵,而非 120 个块。
    (intstatus, true)
}

/// 按厂商 V3 驱动逻辑(radxa aicwf_sdio.c hal_irqhandler 的 D80 分支)解析一帧的
/// 真实字节长度。返回 0 表示无数据。
///
/// V3(D80)逻辑:
///   intmaskf2 = intstatus | (1<<3)
///   if intmaskf2 > 120:        // func2 队列
///       if intmaskf2 == 127:   byte mode -> data_len = reg[0x05] * 4
///       else:                  block mode -> data_len = (intstatus & 0x07) * 512
///   else:                      // func1 队列
///       if intstatus == 120:   byte mode -> data_len = reg[0x05] * 4
///       else:                  block mode -> data_len = (intstatus & 0x7F) * 512
///
/// V2(8801)逻辑:data_len = (intstatus & 0x7F) * 512(intstatus<64 直接块模式,
/// 这里统一按块数 ×512,与现有行为一致)。
fn resolve_rx_data_len(bus: &WifiBus, intstatus: u8) -> usize {
    if !bus.transport.is_v3() {
        return (intstatus & BLOCK_COUNT_MASK) as usize * SDIOWIFI_FUNC_BLOCKSIZE;
    }

    let read_bytemode_len = || -> usize {
        match bus.transport.read_byte(1, bus.transport.bytemode_len_reg()) {
            // 厂商 aicwf_sdio_intr_get_len_bytemode:data_len = byte_len * 4
            // (byte_len <= 128 → 最大 512 字节);只读 0x05,不读 0x06(MSB)。
            Ok(byte_len) => byte_len as usize * 4,
            Err(e) => {
                log::error!("[wifi-rx] read bytemode_len failed: {:?}", e);
                0
            }
        }
    };

    // 厂商 aicwf_sdio.c D80 分支(LYU4662/aic8800-sdio-linux-1.0)逐字对应。
    // 注意:block 模式下读完 0x04 必须立刻读 FIFO,中间不能插 CMD52;
    // 只有 byte 模式(120/127 哨兵)才允许读一次 0x05。
    let intmaskf2 = intstatus | (1 << 3);
    if intmaskf2 > 120 {
        // func2 队列
        if intmaskf2 == 127 {
            read_bytemode_len()
        } else {
            (intstatus & 0x07) as usize * SDIOWIFI_FUNC_BLOCKSIZE
        }
    } else {
        // func1 队列
        if intstatus == 120 {
            read_bytemode_len()
        } else {
            (intstatus & 0x7F) as usize * SDIOWIFI_FUNC_BLOCKSIZE
        }
    }
}

/// 读取 FIFO 数据
///
/// `data_len` 是 resolve_rx_data_len 解析出的真实字节长度(block 模式为 512 的整数倍,
/// byte 模式可为任意 ≤512 的字节数)。按 512 字节分段读,避免 1-bit SDIO 模式下单次
/// CMD53 传输 block 数量过多导致 SDHCI 控制器超时;最后不足 512 的尾段以 byte 模式读
/// (底层 read_fifo 对非 512 对齐长度自动走 byte-mode CMD53)。
///
/// `func`:DC 的 CFM/indication 邮箱在 func2,数据帧在 func1,需按队列读对应 func 的
/// FIFO;8801/D80 命令与数据同在 func1。
fn read_fifo_data(bus: &WifiBus, func: u8, data_len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; data_len];
    let mut offset = 0;

    while offset < data_len {
        let chunk_len = core::cmp::min(data_len - offset, SDIOWIFI_FUNC_BLOCKSIZE);

        if let Err(e) = bus.transport.read_fifo(
            func,
            bus.transport.rd_fifo_addr(),
            &mut buf[offset..offset + chunk_len],
        ) {
            log::error!(
                "[wifi-rx] read_fifo failed at offset {}/{}: {:?}",
                offset,
                data_len,
                e
            );
            return None;
        }

        offset += chunk_len;
    }

    Some(buf)
}

/// 读取 SDIO FIFO 中的所有帧并按类型分发
///
/// 注意：调用方负责在适当时候 unmask CARD_INT（不在本函数内 unmask），
/// 以避免 unmask 和 waker 注册之间的竞态窗口。
fn process_rx_frames(bus: &WifiBus) {
    // ISR 只设 flag 不 mask，这里先 mask CARD_INT 防止重入
    bus.transport.mask_card_irq();

    // 排空 RX FIFO。DC/DW 是双管道:func2 是命令 CFM/indication 邮箱,func1 是数据
    // 平面;两个 func 的 block_cnt 独立,需各自排空(EAPOL M1 等入站数据帧在 func1)。
    // 8801/D80 命令与数据同在 func1,func2 不是独立邮箱——只排空 func1,避免对 func2
    // 发无谓的 CMD52(cmd_func() 对 DC/DW=2,其余=1)。
    if bus.transport.cmd_func() == 2 {
        drain_func(bus, 2);
    }
    drain_func(bus, 1);
}

/// 排空指定 func 的 RX FIFO,读出所有待处理帧并分发。
fn drain_func(bus: &WifiBus, func: u8) {
    let mut other_int_retries = 0u32;

    loop {
        // 在轮询循环中也检查 rx_irq_pending
        if bus.rx.irq_pending.swap(false, Ordering::AcqRel) {
            // ISR 触发了，继续读取（不 break）
        }

        let (intstatus, should_continue) =
            read_block_count_with_retry(bus, func, &mut other_int_retries);
        if !should_continue {
            break;
        }

        if intstatus == 0 {
            break;
        }

        other_int_retries = 0;

        // 按厂商 V3 逻辑解析真实字节长度(区分 func1/func2 + byte/block 模式)
        let data_len = resolve_rx_data_len(bus, intstatus);
        if data_len == 0 {
            break;
        }

        log::trace!(
            "[wifi-rx] intstatus=0x{:02x}, data_len={} bytes, reading FIFO",
            intstatus,
            data_len
        );

        // 读取 FIFO 数据
        let Some(buf) = read_fifo_data(bus, func, data_len) else {
            break;
        };

        dispatch_frames(bus, &buf);
    }
}

// ===== DATA 帧处理辅助函数 =====

/// 硬件接收头部信息
struct HwRxHdrInfo {
    decr_status: u8,
    is_80211_npdu: bool,
}

/// 802.11 地址信息
struct AddrInfo<'a> {
    da: &'a [u8],
    sa: &'a [u8],
}

/// 从硬件接收头部提取信息
fn extract_hw_rxhdr_info(data_payload: &[u8]) -> HwRxHdrInfo {
    const HWVECT_STATUS_OFFSET: usize = 36;
    const FLAGS_OFFSET: usize = 48;
    const DECR_UNENC: u8 = 0;

    let decr_status = if data_payload.len() > HWVECT_STATUS_OFFSET {
        (data_payload[HWVECT_STATUS_OFFSET] >> 2) & 0x07
    } else {
        DECR_UNENC
    };

    let flags_byte0 = if data_payload.len() > FLAGS_OFFSET {
        data_payload[FLAGS_OFFSET]
    } else {
        0
    };
    let is_80211_npdu = (flags_byte0 >> 1) & 0x01 != 0;

    HwRxHdrInfo {
        decr_status,
        is_80211_npdu,
    }
}

/// 检查是否为 802.11 数据帧
fn is_80211_data_frame(fc0: u8) -> bool {
    (fc0 & 0x0C) == 0x08
}

/// 检查是否为 802.11 管理帧 (type=00)
fn is_80211_mgmt_frame(fc0: u8) -> bool {
    (fc0 & 0x0C) == 0x00
}

/// 管理帧子类型 (fc0 高 4 位) 的可读名称
fn mgmt_subtype_name(fc0: u8) -> &'static str {
    match (fc0 >> 4) & 0x0F {
        0x0 => "AssocReq",
        0x1 => "AssocResp",
        0x2 => "ReassocReq",
        0x3 => "ReassocResp",
        0x4 => "ProbeReq",
        0x5 => "ProbeResp",
        0x8 => "Beacon",
        0xA => "Disassoc",
        0xB => "Auth",
        0xC => "Deauth",
        0xD => "Action",
        _ => "Other",
    }
}

/// AP 模式：处理固件转发上来的管理帧。
///
/// 当前实现开放网络的 auth 握手:收到 Auth Request(alg=0,seq=1) 即回
/// Auth Response(alg=0,seq=2,status=0)。Assoc 等后续帧先记录。
fn handle_mgmt_frame(bus: &WifiBus, mpdu: &[u8], pkt_len: usize) {
    let fc0 = mpdu[0];
    let subtype = mgmt_subtype_name(fc0);

    // 管理帧地址布局固定：addr1=DA@4, addr2=SA@10, addr3=BSSID@16
    if pkt_len < 16 {
        return;
    }
    let sa = &mpdu[10..16];

    match (fc0 >> 4) & 0x0F {
        // Auth: alg(2)@24 + seq(2)@26 + status(2)@28
        0xB if pkt_len >= 30 => {
            let alg = u16::from_le_bytes([mpdu[24], mpdu[25]]);
            let seq = u16::from_le_bytes([mpdu[26], mpdu[27]]);
            log::debug!("[ap-rx] Auth from {:02x?}: alg={} seq={}", sa, alg, seq);

            // 开放认证(alg=0)、Auth Request(seq=1) → 回 Auth Response
            if alg == 0 && seq == 1 {
                send_auth_response(bus, sa);
            }
        }
        // Assoc/Reassoc Req → 交给 AP worker 线程处理(ME_STA_ADD + Assoc Resp)
        0x0 | 0x2 if pkt_len >= 28 => {
            let cap = u16::from_le_bytes([mpdu[24], mpdu[25]]);
            log::debug!("[ap-rx] {} from {:02x?}: cap=0x{:04x}", subtype, sa, cap);
            // 不能在 RX 线程做 ME_STA_ADD(send_cmd 会死锁)，整帧入队转给 AP worker
            bus.ap
                .assoc_queue
                .lock()
                .push_back(mpdu[..pkt_len].to_vec());
            bus.ap.assoc_pollset.wake();
        }
        // Deauth(0xC)/Disassoc(0xA):STA 断开,从注册表移除,使重连能完整重新
        // 注册(否则去重会跳过 ME_STA_ADD/控制端口,导致重连连不上)。
        0xC | 0xA => {
            let mut mac = [0u8; 6];
            mac.copy_from_slice(sa);
            let removed = {
                let mut tbl = bus.ap.registered_stas.lock();
                let before = tbl.len();
                tbl.retain(|(m, ..)| *m != mac);
                before != tbl.len()
            };
            log::info!(
                "[ap-rx] {} from {:02x?} (removed_from_table={})",
                subtype,
                sa,
                removed
            );
        }
        _ => {
            // Beacon / ProbeReq 等：周围 AP 和扫描设备的帧，与连接无关。
            // 降为 trace，避免淹没握手/数据帧日志。
            log::trace!("[ap-rx] {} from {:02x?} (fc0=0x{:02x})", subtype, sa, fc0);
        }
    }
}

/// 构造并发送开放网络 Auth Response (alg=0, seq=2, status=0)。
fn send_auth_response(bus: &WifiBus, dst: &[u8]) {
    let ap_mac = match *bus.conn.sta_mac.lock() {
        Some(m) => m,
        None => {
            log::warn!("[ap-rx] no AP mac, cannot send Auth Response");
            return;
        }
    };

    // 802.11 Auth 帧: mgmt 头(24) + alg(2) + seq(2) + status(2) = 30 字节
    let mut frame = Vec::with_capacity(30);
    frame.extend_from_slice(&[0xB0, 0x00]); // fc: mgmt, subtype=Auth(0xB)
    frame.extend_from_slice(&[0x00, 0x00]); // duration
    frame.extend_from_slice(dst); // addr1 = DA (手机)
    frame.extend_from_slice(&ap_mac); // addr2 = SA (AP)
    frame.extend_from_slice(&ap_mac); // addr3 = BSSID
    frame.extend_from_slice(&[0x00, 0x00]); // seq ctrl (固件填)
    frame.extend_from_slice(&0u16.to_le_bytes()); // auth algorithm = Open(0)
    frame.extend_from_slice(&2u16.to_le_bytes()); // auth seq = 2
    frame.extend_from_slice(&0u16.to_le_bytes()); // status = success(0)

    match crate::fdrv::thread::tx::enqueue_mgmt_frame(bus, frame) {
        Ok(()) => log::debug!("[ap-tx] Auth Response queued -> {:02x?}", dst),
        Err(e) => log::warn!("[ap-tx] Auth Response enqueue failed: {:?}", e),
    }
}

/// 获取 802.11 头部长度
fn get_80211_header_len(fc0: u8, fc1: u8) -> usize {
    let is_qos = (fc0 & 0x80) != 0;
    let mut hdr_len: usize = if is_qos { 26 } else { 24 };
    if (fc1 & 0x80) != 0 {
        hdr_len += 4; // +HTC
    }
    hdr_len
}

/// 解析 802.11 地址信息
fn parse_80211_addrs(mpdu: &[u8], fc1: u8, pkt_len: usize) -> Option<AddrInfo<'_>> {
    let to_ds = fc1 & 0x01;
    let from_ds = (fc1 >> 1) & 0x01;

    let (da, sa): (&[u8], &[u8]) = match (to_ds, from_ds) {
        (0, 0) => (&mpdu[4..10], &mpdu[10..16]),
        (1, 0) => (&mpdu[16..22], &mpdu[10..16]),
        (0, 1) => (&mpdu[4..10], &mpdu[16..22]),
        _ => {
            if pkt_len < 30 {
                return None;
            }
            (&mpdu[16..22], &mpdu[24..30])
        }
    };

    Some(AddrInfo { da, sa })
}

/// 获取加密头长度
fn get_crypto_header_len(decr_status: u8) -> usize {
    const DECR_CCMP128: u8 = 3;
    const DECR_CCMP256: u8 = 4;
    const DECR_GCMP128: u8 = 5;
    const DECR_GCMP256: u8 = 6;
    const DECR_TKIP: u8 = 2;
    const DECR_WEP: u8 = 1;
    const DECR_WAPI: u8 = 7;

    match decr_status {
        DECR_CCMP128 | DECR_CCMP256 | DECR_GCMP128 | DECR_GCMP256 => 8,
        DECR_TKIP => 8,
        DECR_WEP => 4,
        DECR_WAPI => 18,
        _ => 0,
    }
}

/// 提取以太网类型
fn extract_ethertype(mpdu: &[u8], ether_type_offset: usize, pkt_len: usize) -> Option<u16> {
    if pkt_len < ether_type_offset + 2 {
        log::trace!(
            "[wifi-rx] MPDU too short for LLC/SNAP: pkt_len={}, need={}",
            pkt_len,
            ether_type_offset + 2
        );
        return None;
    }

    Some(u16::from_be_bytes([
        mpdu[ether_type_offset],
        mpdu[ether_type_offset + 1],
    ]))
}

/// 处理 EAPOL 帧
fn process_eapol_frame(bus: &WifiBus, mpdu: &[u8], payload_start: usize, pkt_len: usize) {
    if pkt_len <= payload_start {
        return;
    }

    let raw_eapol = &mpdu[payload_start..];

    let eapol = if raw_eapol.len() >= 4 {
        let body_len = u16::from_be_bytes([raw_eapol[2], raw_eapol[3]]) as usize;
        let actual_len = 4 + body_len;
        if actual_len <= raw_eapol.len() {
            raw_eapol[..actual_len].to_vec()
        } else {
            raw_eapol.to_vec()
        }
    } else {
        raw_eapol.to_vec()
    };

    let mut queue = bus.rx.eapol_queue.lock();
    queue.push_back(eapol);
    drop(queue);
    bus.rx.eapol_pollset.wake();
}

/// 构造并发送以太网帧
fn build_and_enqueue_eth_frame(
    bus: &WifiBus,
    mpdu: &[u8],
    addr_info: &AddrInfo<'_>,
    ether_type_offset: usize,
    payload_start: usize,
    pkt_len: usize,
) {
    const DATA_RX_QUEUE_MAX: usize = 64;

    if pkt_len <= payload_start {
        return;
    }

    let payload = &mpdu[payload_start..];
    let mut eth_frame = Vec::with_capacity(14 + payload.len());
    eth_frame.extend_from_slice(addr_info.da);
    eth_frame.extend_from_slice(addr_info.sa);
    eth_frame.extend_from_slice(&mpdu[ether_type_offset..ether_type_offset + 2]);
    eth_frame.extend_from_slice(payload);

    // AP 模式数据帧(ARP/DHCP/IP):正常路径,降为 trace 避免淹没日志。
    let et = u16::from_be_bytes([mpdu[ether_type_offset], mpdu[ether_type_offset + 1]]);
    log::trace!(
        "[ap-rx] DATA from {:02x?} ethertype=0x{:04x} len={}",
        addr_info.sa,
        et,
        eth_frame.len()
    );

    let mut queue = bus.rx.data_queue.lock();
    if queue.len() >= DATA_RX_QUEUE_MAX {
        queue.pop_front();
    }
    queue.push_back(eth_frame);
    drop(queue);
    bus.rx.data_pollset.wake();
    // 标记本批有数据帧入队,RX 线程稍后会驱动网络栈 poll。
    RX_DATA_PENDING.store(true, Ordering::Release);
}

/// 处理单个数据帧
fn process_data_frame(bus: &WifiBus, data_payload: &[u8], pkt_len: usize, _mpdu_offset: usize) {
    const MPDU_OFFSET: usize = 60;

    if pkt_len < 24 || data_payload.len() < MPDU_OFFSET + pkt_len {
        log::warn!("[wifi-rx] DATA frame too short for 802.11 header");
        return;
    }

    let mpdu = &data_payload[MPDU_OFFSET..MPDU_OFFSET + pkt_len];
    let fc0 = mpdu[0];
    let fc1 = mpdu[1];

    // 802.11 帧控制字段(data/mgmt/子类型)。trace 级:AP 模式下每个周围
    // beacon 都会进来,info 刷屏会拖死 RX 线程、错过 Auth/Assoc 握手。
    log::trace!(
        "[wifi-rx] 80211 fc0=0x{:02x} fc1=0x{:02x} data={} mgmt={} pkt_len={}",
        fc0,
        fc1,
        is_80211_data_frame(fc0),
        is_80211_mgmt_frame(fc0),
        pkt_len
    );

    if !is_80211_data_frame(fc0) {
        // AP 模式：管理帧(Auth/Assoc 等)由固件转发上来，处理握手。
        if is_80211_mgmt_frame(fc0) {
            handle_mgmt_frame(bus, mpdu, pkt_len);
        }
        return;
    }

    let hdr_len = get_80211_header_len(fc0, fc1);

    let addr_info = match parse_80211_addrs(mpdu, fc1, pkt_len) {
        Some(info) => info,
        None => return,
    };

    let hw_info = extract_hw_rxhdr_info(data_payload);

    if hw_info.is_80211_npdu {
        return;
    }

    let crypto_hdr_len = get_crypto_header_len(hw_info.decr_status);

    let llc_offset = hdr_len + crypto_hdr_len;
    let ether_type_offset = llc_offset + 6;

    let ethertype = match extract_ethertype(mpdu, ether_type_offset, pkt_len) {
        Some(et) => et,
        None => return,
    };

    let payload_start = llc_offset + 8;

    log::trace!(
        "[wifi-rx] DATA ethertype=0x{:04x} (EAPOL={}) hdr_len={} crypto={} pkt_len={}",
        ethertype,
        ethertype == ETH_P_PAE,
        hdr_len,
        crypto_hdr_len,
        pkt_len
    );

    if ethertype == ETH_P_PAE {
        process_eapol_frame(bus, mpdu, payload_start, pkt_len);
    } else {
        build_and_enqueue_eth_frame(
            bus,
            mpdu,
            &addr_info,
            ether_type_offset,
            payload_start,
            pkt_len,
        );
    }
}

// ===== CFG 帧处理辅助函数 =====

/// 处理 CMD_RSP 类型的 CFG 帧
fn process_cmd_rsp(bus: &WifiBus, msg_data: &[u8]) {
    if msg_data.len() < 8 {
        log::warn!(
            "[wifi-rx] process_cmd_rsp: msg_data too short ({})",
            msg_data.len()
        );
        return;
    }

    let msg_id = u16::from_le_bytes([msg_data[0], msg_data[1]]);

    let expected_cfm = bus.cmd.expected_cfm_id.load(Ordering::Acquire);

    if expected_cfm != 0 && msg_id == expected_cfm {
        log::debug!("[wifi-rx] CFM match: msg_id=0x{:04x} -> rsp_queue", msg_id);
        let mut queue = bus.cmd.rsp_queue.lock();
        queue.push_back(msg_data.to_vec());
        drop(queue);
        bus.cmd.rsp_pollset.wake();
    } else {
        log::debug!(
            "[wifi-rx] indication: msg_id=0x{:04x} (expected_cfm=0x{:04x}) -> ind_queue",
            msg_id,
            expected_cfm
        );
        let mut queue = bus.tx.ind_queue.lock();
        queue.push_back(msg_data.to_vec());
        drop(queue);
        bus.tx.ind_pollset.wake();
    }
}

/// 处理 PRINT 类型的 CFG 帧
fn process_print_frame(msg_data: &[u8]) {
    if let Ok(s) = core::str::from_utf8(msg_data) {
        log::info!("[fw-print] {}", s.trim_end_matches('\0'));
    }
}

/// 处理 CFG 帧
fn process_cfg_frame(bus: &WifiBus, msg_data: &[u8], cfg_subtype: u8) {
    match cfg_subtype {
        SDIO_TYPE_CFG_CMD_RSP => {
            process_cmd_rsp(bus, msg_data);
        }
        SDIO_TYPE_CFG_DATA_CFM => {
            log::debug!("[wifi-rx] DATA_CFM received, len={}", msg_data.len());
        }
        SDIO_TYPE_CFG_PRINT => {
            process_print_frame(msg_data);
        }
        _ => {
            log::warn!(
                "[wifi-rx] unknown frame type=0x{:02x}, len={}",
                cfg_subtype,
                msg_data.len()
            );
        }
    }
}

// ===== 主分发函数 =====

/// 解析 SDIO FIFO 中的聚合帧并按类型分发
fn dispatch_frames(bus: &WifiBus, buf: &[u8]) {
    let mut offset = 0;

    while offset + 4 <= buf.len() {
        let pkt_len = u16::from_le_bytes([buf[offset], buf[offset + 1]]) as usize;
        if pkt_len == 0 || pkt_len > MAX_PKT_LEN as usize {
            break;
        }

        let pkt_type = buf[offset + 2] & 0x7F;
        let is_cfg = (pkt_type & SDIO_TYPE_CFG) == SDIO_TYPE_CFG;

        // 跳过 PRINT(idle 刷屏的固件 debug 帧)。trace 级:AP 模式每个周围
        // beacon 都会进来,info 刷屏会拖死 RX 线程。
        if !(is_cfg && pkt_type == SDIO_TYPE_CFG_PRINT) {
            log::trace!(
                "[wifi-rx] frame off={} pkt_len={} type=0x{:02x} is_cfg={}",
                offset,
                pkt_len,
                pkt_type,
                is_cfg
            );
        }

        if !is_cfg {
            // ========== DATA 帧 ==========
            log::trace!(
                "[RXDIAG] DATA frame: pkt_type=0x{:02x} pkt_len={} offset={}",
                pkt_type,
                pkt_len,
                offset
            );
            let aggr_len = pkt_len + RX_HWHRD_LEN;
            let advance = align_up(aggr_len, RX_ALIGNMENT);

            if offset + aggr_len > buf.len() {
                log::warn!("[wifi-rx] DATA frame truncated at offset={}", offset);
                break;
            }

            let data_payload = &buf[offset..offset + aggr_len];
            process_data_frame(bus, data_payload, pkt_len, 60);

            offset += advance;
        } else {
            // ========== CFG 帧 ==========
            let msg_start = offset + 4;
            let msg_end = msg_start + pkt_len;

            if msg_end > buf.len() {
                log::warn!("[wifi-rx] CFG frame truncated at offset={}", offset);
                break;
            }

            let msg_data = &buf[msg_start..msg_end];
            let cfg_subtype = pkt_type;

            let advance = align_up(pkt_len, RX_ALIGNMENT) + 4;
            process_cfg_frame(bus, msg_data, cfg_subtype);

            offset += advance;
        }
    }
}
