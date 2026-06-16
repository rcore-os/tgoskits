//! CMD 发送框架和 EAPOL 发送
//!
//! 提供 LMAC 命令发送/等待 CFM 的核心机制，
//! 以及 EAPOL 帧构造和发送。
//!
//! 芯片配置命令 → config.rs
//! 扫描命令 → scan.rs
//! 连接/断连命令 → connection.rs
//! 密钥命令 → key.rs

use alloc::{sync::Arc, vec, vec::Vec};
use core::{sync::atomic::Ordering, task::Poll};

use crate::{
    common::SDIO_TYPE_CFG_CMD_RSP,
    fdrv::{
        consts::*,
        core::bus::{BusState, WifiBus},
        protocol::lmac_msg::*,
    },
    runtime::runtime,
};

// ===== 辅助函数（子模块共享） =====

pub(crate) fn current_time_ms() -> u64 {
    crate::runtime::runtime().now_nanos() / 1_000_000
}

fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

// ===== CMD 帧构造和发送 =====

/// 构造 SDIO CMD 帧
fn build_cmd_frame(msg_id: u16, dest_task: u16, param: &[u8], is_v3: bool) -> Vec<u8> {
    let lmac_len = 8 + param.len();
    let raw_len = 4 + DUMMY_WORD_LEN + lmac_len;
    let sdio_len = raw_len;

    let aligned_len = align_up(raw_len, TX_ALIGNMENT);

    let final_len = if !aligned_len.is_multiple_of(SDIOWIFI_FUNC_BLOCKSIZE) {
        align_up(aligned_len + TAIL_LEN, SDIOWIFI_FUNC_BLOCKSIZE)
    } else {
        aligned_len
    };

    let mut buf = vec![0u8; final_len];

    // sdio_header [0..4]
    buf[0] = (sdio_len & 0xFF) as u8;
    buf[1] = ((sdio_len >> 8) & 0x0F) as u8;
    buf[2] = SDIO_TYPE_CFG_CMD_RSP;
    buf[3] = if is_v3 {
        crate::common::crc8_ponl_107(&buf[0..3])
    } else {
        0x00
    };

    // lmac_msg header [8..16]
    let msg_offset = 4 + DUMMY_WORD_LEN;
    buf[msg_offset..msg_offset + 2].copy_from_slice(&msg_id.to_le_bytes());
    buf[msg_offset + 2..msg_offset + 4].copy_from_slice(&dest_task.to_le_bytes());
    buf[msg_offset + 4..msg_offset + 6].copy_from_slice(&DRV_TASK_ID.to_le_bytes());
    buf[msg_offset + 6..msg_offset + 8].copy_from_slice(&(param.len() as u16).to_le_bytes());

    if !param.is_empty() {
        buf[msg_offset + 8..msg_offset + 8 + param.len()].copy_from_slice(param);
    }

    buf
}

/// 准备 CMD 发送（清空队列、设置标志）
fn prepare_cmd_send(bus: &Arc<WifiBus>, expected_cfm_id: u16) {
    {
        let mut queue = bus.cmd.rsp_queue.lock();
        if !queue.is_empty() {
            log::warn!("[cmd_mgr] discarding {} stale CMD responses", queue.len());
            queue.clear();
        }
    }
    bus.cmd.rsp_error.store(false, Ordering::Release);
    bus.cmd
        .expected_cfm_id
        .store(expected_cfm_id, Ordering::Release);
}

/// 通过 TX 线程发送 CMD 帧
fn send_cmd_via_tx_thread(bus: &Arc<WifiBus>, frame: Vec<u8>) {
    let mut cmd_slot = bus.cmd.pending.lock();
    *cmd_slot = Some(frame);
    bus.cmd.pending_flag.store(true, Ordering::Release);
    drop(cmd_slot);
    bus.tx.wake_pollset.wake();
}

/// 验证 CFM 并提取参数
///
/// 固件响应用 `struct ipc_e2a_msg`（12字节 header 含 pattern 字段），
/// param 从 offset 12 开始（msg_id(2) + dest_id(2) + src_id(2) + param_len(2) + pattern(4)）。
fn validate_and_extract_cfm_param(rsp: &[u8], _expected_cfm_id: u16) -> Result<Vec<u8>, ()> {
    if rsp.len() < 12 {
        return Err(());
    }

    let msg = LmacMsg::from_le_bytes(rsp);
    log::debug!(
        "[cmd_mgr] CFM received: msg_id=0x{:04x}, param_len={}",
        msg.id,
        msg.param_len
    );

    let param_start = 12;
    let param_end = param_start + msg.param_len as usize;

    if rsp.len() >= param_end {
        Ok(rsp[param_start..param_end].to_vec())
    } else {
        Ok(rsp[param_start..].to_vec())
    }
}

fn try_get_cfm_from_queue(
    bus: &Arc<WifiBus>,
    expected_cfm_id: u16,
) -> Option<Result<Vec<u8>, CmdError>> {
    let mut queue = bus.cmd.rsp_queue.lock();
    let mut redirected = Vec::new();

    while let Some(rsp) = queue.pop_front() {
        if rsp.len() < LmacMsg::SIZE {
            continue;
        }
        let msg = LmacMsg::from_le_bytes(&rsp);
        if msg.id == expected_cfm_id {
            let _ = redirected;
            drop(queue);
            if !redirected.is_empty() {
                let mut ind = bus.tx.ind_queue.lock();
                for r in redirected {
                    ind.push_back(r);
                }
                bus.tx.ind_pollset.wake();
            }
            return Some(Ok(rsp[LmacMsg::SIZE..].to_vec()));
        } else {
            redirected.push(rsp);
        }
    }

    if !redirected.is_empty() {
        let mut ind = bus.tx.ind_queue.lock();
        for r in redirected {
            ind.push_back(r);
        }
        bus.tx.ind_pollset.wake();
    }
    None
}

/// 异步等待 CFM 响应（不含超时逻辑，由外部 timeout 包装器提供）
/// 轮询体：等待指定 CFM ID 的响应。返回 `Poll::Ready` 携带结果。
fn poll_cfm_response(
    bus: &Arc<WifiBus>,
    expected_cfm_id: u16,
    out: &mut Option<Result<Vec<u8>, CmdError>>,
    cx: &mut core::task::Context<'_>,
) -> Poll<()> {
    if bus.cmd.rsp_error.load(Ordering::Acquire) || *bus.state.lock() == BusState::Down {
        *out = Some(Err(CmdError::BusDown));
        return Poll::Ready(());
    }

    if let Some(result) = try_get_cfm_from_queue(bus, expected_cfm_id) {
        *out = Some(result);
        return Poll::Ready(());
    }

    // 注册 waker，等待 RX 线程通过 rsp_pollset.wake() 唤醒
    bus.cmd.rsp_pollset.register(cx.waker());

    // 双重检查
    if let Some(result) = try_get_cfm_from_queue(bus, expected_cfm_id) {
        *out = Some(result);
        return Poll::Ready(());
    }

    Poll::Pending
}

// ===== 公共 CMD API =====

/// 发送 LMAC 命令并等待 CFM
pub fn send_cmd(
    bus: &Arc<WifiBus>,
    msg_id: u16,
    dest_id: u16,
    param: &[u8],
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    send_cmd_with_cfm_id(bus, msg_id, dest_id, param, msg_id + 1, timeout_ms)
}

/// 发送 LMAC 命令并等待指定 CFM ID 的响应
pub fn send_cmd_with_cfm_id(
    bus: &Arc<WifiBus>,
    msg_id: u16,
    dest_id: u16,
    param: &[u8],
    expected_cfm_id: u16,
    timeout_ms: u64,
) -> Result<Vec<u8>, CmdError> {
    if *bus.state.lock() == BusState::Down {
        return Err(CmdError::BusDown);
    }

    let tout = if timeout_ms == 0 {
        CMD_TX_TIMEOUT_DEFAULT_MS
    } else {
        timeout_ms
    };

    let frame = build_cmd_frame(msg_id, dest_id, param, bus.transport.is_v3());
    prepare_cmd_send(bus, expected_cfm_id);
    send_cmd_via_tx_thread(bus, frame);

    // 通过注入的 runtime 阻塞等待：同时由 rsp_pollset 唤醒和超时定时器驱动。
    let mut cfm: Option<Result<Vec<u8>, CmdError>> = None;
    let result = match runtime().block_until(Some(tout), &mut |cx| {
        poll_cfm_response(bus, expected_cfm_id, &mut cfm, cx)
    }) {
        Ok(()) => cfm.unwrap_or(Err(CmdError::Timeout)),
        Err(_) => {
            log::error!(
                "[cmd_mgr] TIMEOUT waiting for cfm 0x{:04x} ({}ms)",
                expected_cfm_id,
                tout
            );
            Err(CmdError::Timeout)
        }
    };

    bus.cmd.expected_cfm_id.store(0, Ordering::Release);

    if let Err(e) = &result {
        log::error!("[cmd_mgr] send_cmd 0x{:04x} error: {:?}", msg_id, e);
    }

    result
}

/// 发送命令不等待 CFM
pub fn send_cmd_no_cfm(
    bus: &Arc<WifiBus>,
    msg_id: u16,
    dest_id: u16,
    param: &[u8],
) -> Result<(), CmdError> {
    if *bus.state.lock() == BusState::Down {
        return Err(CmdError::BusDown);
    }

    let frame = build_cmd_frame(msg_id, dest_id, param, bus.transport.is_v3());
    {
        let mut cmd_slot = bus.cmd.pending.lock();
        *cmd_slot = Some(frame);
        bus.cmd.pending_flag.store(true, Ordering::Release);
    }
    bus.tx.wake_pollset.wake();

    Ok(())
}

// ===== EAPOL 发送 =====

/// 轮询体：从 eapol_queue 取出 EAPOL 帧。
fn poll_eapol(
    bus: &Arc<WifiBus>,
    out: &mut Option<Vec<u8>>,
    cx: &mut core::task::Context<'_>,
) -> Poll<()> {
    {
        let mut queue = bus.rx.eapol_queue.lock();
        if let Some(eapol) = queue.pop_front() {
            queue.clear();
            *out = Some(eapol);
            return Poll::Ready(());
        }
    }

    bus.rx.eapol_pollset.register(cx.waker());

    {
        let mut queue = bus.rx.eapol_queue.lock();
        if let Some(eapol) = queue.pop_front() {
            queue.clear();
            *out = Some(eapol);
            return Poll::Ready(());
        }
    }

    Poll::Pending
}

/// 等待 EAPOL 帧（从 eapol_queue 中取出）
pub fn wait_for_eapol(bus: &Arc<WifiBus>, timeout_ms: u64) -> Result<Vec<u8>, CmdError> {
    let mut eapol: Option<Vec<u8>> = None;
    match runtime().block_until(Some(timeout_ms), &mut |cx| poll_eapol(bus, &mut eapol, cx)) {
        Ok(()) => match eapol {
            Some(e) => Ok(e),
            None => {
                log::error!("[cmd_mgr] EAPOL wait returned empty");
                Err(CmdError::Timeout)
            }
        },
        Err(_) => {
            log::error!("[cmd_mgr] EAPOL wait timed out");
            Err(CmdError::Timeout)
        }
    }
}

struct EapolFrameLayout {
    final_len: usize,
    sdio_hdr_len: usize,
    payload_len: usize,
}

fn calculate_eapol_frame_layout(eapol_len: usize) -> EapolFrameLayout {
    const SDIO_HEADER_LEN: usize = 4;
    const HOSTDESC_SIZE: usize = 28;

    let payload_len = eapol_len;
    let sdio_payload_len = HOSTDESC_SIZE + payload_len;
    let sdio_hdr_len = SDIO_HEADER_LEN + sdio_payload_len;
    let aligned = align_up(sdio_hdr_len, TX_ALIGNMENT);
    let final_len = if !aligned.is_multiple_of(SDIOWIFI_FUNC_BLOCKSIZE) {
        let with_tail = aligned + 4;
        ((with_tail / SDIOWIFI_FUNC_BLOCKSIZE) + 1) * SDIOWIFI_FUNC_BLOCKSIZE
    } else {
        aligned
    };

    EapolFrameLayout {
        final_len,
        sdio_hdr_len,
        payload_len,
    }
}

fn fill_sdio_header_for_eapol(buf: &mut [u8], layout: &EapolFrameLayout, is_v3: bool) {
    buf[0] = (layout.sdio_hdr_len & 0xFF) as u8;
    buf[1] = ((layout.sdio_hdr_len >> 8) & 0x0F) as u8;
    buf[2] = 0x01; // SDIO_TYPE_DATA
    // V3(D80)固件校验 SDIO header 第 4 字节的 CRC8;V2(8801)恒为 0。
    // 与 build_data_frame/build_cmd_frame 的逻辑保持一致——之前这里漏了 V3
    // 分支,EAPOL 帧在 D80 上因 CRC 校验失败被固件静默丢弃,导致 M2 发不出去。
    buf[3] = if is_v3 {
        crate::common::crc8_ponl_107(&buf[0..3])
    } else {
        0x00
    };
}

fn fill_hostdesc_for_eapol(
    hd: &mut [u8],
    payload_len: usize,
    dst_mac: &[u8; 6],
    src_mac: &[u8; 6],
    vif_idx: u8,
    sta_idx: u8,
) {
    hd[0..2].copy_from_slice(&(payload_len as u16).to_le_bytes());
    hd[4..8].copy_from_slice(&0x8000_0001u32.to_le_bytes());
    hd[8..14].copy_from_slice(dst_mac);
    hd[14..20].copy_from_slice(src_mac);
    hd[20..22].copy_from_slice(&0x888Eu16.to_be_bytes());
    hd[24] = vif_idx;
    hd[25] = sta_idx;
}

fn build_eapol_frame_buffer(
    dst_mac: &[u8; 6],
    src_mac: &[u8; 6],
    eapol: &[u8],
    vif_idx: u8,
    sta_idx: u8,
    is_v3: bool,
) -> Vec<u8> {
    const SDIO_HEADER_LEN: usize = 4;
    const HOSTDESC_SIZE: usize = 28;

    let layout = calculate_eapol_frame_layout(eapol.len());
    let mut buf = vec![0u8; layout.final_len];

    fill_sdio_header_for_eapol(&mut buf, &layout, is_v3);
    fill_hostdesc_for_eapol(
        &mut buf[SDIO_HEADER_LEN..SDIO_HEADER_LEN + HOSTDESC_SIZE],
        layout.payload_len,
        dst_mac,
        src_mac,
        vif_idx,
        sta_idx,
    );

    let eth_start = SDIO_HEADER_LEN + HOSTDESC_SIZE;
    buf[eth_start..eth_start + eapol.len()].copy_from_slice(eapol);

    buf
}

/// 执行 EAPOL 流控检查
fn perform_eapol_flow_control(bus: &WifiBus) -> Result<(), CmdError> {
    if bus.transport.wait_flow_ctrl(50) {
        Ok(())
    } else {
        log::error!("[cmd_mgr] EAPOL TX flow_ctrl timeout");
        Err(CmdError::Timeout)
    }
}

/// 发送 EAPOL 帧到 SDIO
fn send_eapol_frame_to_sdio(bus: &Arc<WifiBus>, buf: &[u8]) -> Result<(), CmdError> {
    let transport = &bus.transport;

    transport.mask_card_irq();

    if let Err(e) = perform_eapol_flow_control(bus) {
        transport.unmask_card_irq();
        return Err(e);
    }

    // V3(D80)的写 FIFO 地址是 0x10,V2(8801)是 0x07。必须用 transport 的
    // V3 感知方法,与 CMD/DATA/MGMT 帧路径一致——之前这里写死了 V2 常量,
    // 导致 D80 上 M2 被写到错误 SDIO 地址,固件收不到,4 次握手卡死。
    if let Err(e) = transport.write_fifo(1, transport.wr_fifo_addr(), buf) {
        log::error!("[cmd_mgr] EAPOL TX write_fifo failed: {:?}", e);
        transport.unmask_card_irq();
        return Err(CmdError::SdioError);
    }

    bus.rx.irq_waker.wake();
    transport.unmask_card_irq();

    Ok(())
}

/// 发送 EAPOL DATA 帧
pub fn send_eapol_data_frame(
    bus: &Arc<WifiBus>,
    dst_mac: &[u8; 6],
    src_mac: &[u8; 6],
    eapol: &[u8],
    vif_idx: u8,
    sta_idx: u8,
) -> Result<(), CmdError> {
    let buf = build_eapol_frame_buffer(
        dst_mac,
        src_mac,
        eapol,
        vif_idx,
        sta_idx,
        bus.transport.is_v3(),
    );
    send_eapol_frame_to_sdio(bus, &buf)
}
