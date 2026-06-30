//! FDRV 初始化模块
//!
//! 包含驱动初始化相关函数

use alloc::{vec, vec::Vec};
use core::sync::atomic::Ordering;

use log::{error, warn};
use sdio_host::SdioHost;

use crate::{
    common::{
        ChipVariant, SDIO_TYPE_CFG_CMD_RSP, SDIOWIFI_BLOCK_CNT_REG, SDIOWIFI_BYTEMODE_ENABLE_REG,
        SDIOWIFI_BYTEMODE_ENABLE_REG_V3, SDIOWIFI_FLOW_CTRL_Q1_REG_V3, SDIOWIFI_FLOW_CTRL_REG,
        SDIOWIFI_FLOWCTRL_MASK, SDIOWIFI_INTR_CONFIG_REG, SDIOWIFI_INTR_ENABLE_REG_V3,
        SDIOWIFI_MISC_INT_STATUS_REG_V3, SDIOWIFI_RD_FIFO_ADDR, SDIOWIFI_RD_FIFO_ADDR_V3,
        SDIOWIFI_REGISTER_BLOCK, SDIOWIFI_SLEEP_REG_V3, SDIOWIFI_V3_SLEEP_READY_BIT,
        SDIOWIFI_V3_WAKEUP_VALUE, SDIOWIFI_WAKEUP_REG_V3, SDIOWIFI_WR_FIFO_ADDR,
        SDIOWIFI_WR_FIFO_ADDR_V3,
    },
    fdrv::{
        consts::*,
        core::{
            bus::{BusState, IRQ_COUNT, WifiBus, set_global_bus},
            sdio_transport::SdioTransport,
        },
        protocol::{DRV_TASK_ID, MM_SET_STACK_START_REQ, TASK_MM},
        thread::{ap, rx, tx},
    },
};

// ===== V2/V3 register helpers =====

fn block_cnt_reg(is_v3: bool) -> u32 {
    if is_v3 {
        SDIOWIFI_MISC_INT_STATUS_REG_V3
    } else {
        SDIOWIFI_BLOCK_CNT_REG
    }
}

fn flow_ctrl_reg(is_v3: bool) -> u32 {
    if is_v3 {
        SDIOWIFI_FLOW_CTRL_Q1_REG_V3
    } else {
        SDIOWIFI_FLOW_CTRL_REG
    }
}

fn rd_fifo_reg(is_v3: bool) -> u32 {
    if is_v3 {
        SDIOWIFI_RD_FIFO_ADDR_V3
    } else {
        SDIOWIFI_RD_FIFO_ADDR
    }
}

fn wr_fifo_reg(is_v3: bool) -> u32 {
    if is_v3 {
        SDIOWIFI_WR_FIFO_ADDR_V3
    } else {
        SDIOWIFI_WR_FIFO_ADDR
    }
}

fn intr_config_reg(is_v3: bool) -> u32 {
    if is_v3 {
        SDIOWIFI_INTR_ENABLE_REG_V3
    } else {
        SDIOWIFI_INTR_CONFIG_REG
    }
}

/// DC/DW 的命令邮箱在 SDIO function 2(与 bootrom 阶段一致, 真机实测 CFM 的
/// block_cnt 在 func2 递增, func1 恒 0)。其余芯片走 func1。
fn cmd_func(chip: ChipVariant) -> u8 {
    if matches!(chip, ChipVariant::Aic8800DC | ChipVariant::Aic8800DW) {
        2
    } else {
        1
    }
}

// ===== polling_send_cmd 辅助函数 =====

/// 计算轮询命令帧的长度和对齐
fn calculate_polling_frame_layout(param_len: usize) -> usize {
    let lmac_len = LMAC_MSG_HEADER_SIZE + param_len;
    let raw_len = SDIO_HEADER_SIZE + DUMMY_WORD_LEN + lmac_len;

    let aligned = (raw_len + TX_ALIGNMENT - 1) & !(TX_ALIGNMENT - 1);
    if !aligned.is_multiple_of(SDIOWIFI_FUNC_BLOCKSIZE) {
        let with_tail = aligned + TAIL_LEN;
        ((with_tail / SDIOWIFI_FUNC_BLOCKSIZE) + 1) * SDIOWIFI_FUNC_BLOCKSIZE
    } else {
        aligned
    }
}

/// 构造轮询命令帧
fn build_polling_cmd_frame(msg_id: u16, dest_id: u16, param: &[u8], is_v3: bool) -> Vec<u8> {
    let lmac_len = LMAC_MSG_HEADER_SIZE + param.len();
    let sdio_payload_len = DUMMY_WORD_LEN + lmac_len;
    let sdio_len = sdio_payload_len + SDIO_HEADER_SIZE;

    let final_len = calculate_polling_frame_layout(param.len());
    let mut buf: Vec<u8> = vec![0u8; final_len];

    // sdio_header [0..4]
    buf[0] = (sdio_len & U8_MASK as usize) as u8;
    buf[1] = ((sdio_len >> 8) & LOW_NIBBLE_MASK as usize) as u8;
    buf[2] = SDIO_TYPE_CFG_CMD_RSP;
    buf[3] = if is_v3 {
        crate::common::crc8_ponl_107(&buf[0..3])
    } else {
        0x00
    };

    // lmac_msg header [8..16]
    let off = SDIO_HEADER_SIZE + DUMMY_WORD_LEN; // = 8
    buf[off..off + 2].copy_from_slice(&msg_id.to_le_bytes());
    buf[off + 2..off + 4].copy_from_slice(&dest_id.to_le_bytes());
    buf[off + 4..off + 6].copy_from_slice(&DRV_TASK_ID.to_le_bytes());
    buf[off + 6..off + 8].copy_from_slice(&(param.len() as u16).to_le_bytes());

    if !param.is_empty() {
        buf[off + 8..off + 8 + param.len()].copy_from_slice(param);
    }

    buf
}

/// 轮询模式流控检查（初始化阶段直接操作 sdio）
///
/// DC/DW(func==2): bootrom 已验证 DC 命令路径不轮询流控寄存器, 直接跳过。
fn check_flow_control_polling<H: SdioHost>(
    sdio: &H,
    is_v3: bool,
    func: u8,
) -> Result<(), &'static str> {
    if func == 2 {
        log::info!("[fdrv] DC: skip flow_ctrl check (cmd on func2)");
        return Ok(());
    }
    for retry in 0..FLOW_CONTROL_MAX_RETRY {
        match sdio.read_byte(1, flow_ctrl_reg(is_v3)) {
            Ok(fc) => {
                let fc_val = fc & SDIOWIFI_FLOWCTRL_MASK;
                if fc_val != 0 {
                    log::info!(
                        "[fdrv] flow_ctrl OK, reg=0x{:02x}, val={} (raw=0x{:02x})",
                        flow_ctrl_reg(is_v3),
                        fc_val,
                        fc
                    );
                    return Ok(());
                }
            }
            Err(_) => return Err("flow_ctrl read error"),
        }
        if retry >= FLOW_CONTROL_MAX_RETRY - 1 {
            return Err("flow_ctrl timeout");
        }
        crate::runtime::runtime().sleep_ms(1);
    }
    Ok(())
}

/// 轮询等待响应
fn poll_for_response<H: SdioHost>(
    sdio: &H,
    is_v3: bool,
    func: u8,
    expected_cfm: u16,
    cfm_buf: &mut [u8],
) -> Result<usize, &'static str> {
    for retry in 0..RESPONSE_MAX_RETRY {
        let raw = sdio
            .read_byte(func, block_cnt_reg(is_v3))
            .map_err(|_| "read block_cnt error")?;

        // DIAG: 每 500 次扫一遍关键寄存器, 定位 CFM 到底回没回/回在哪个 func。
        // 仅在 trace 开启时执行,避免生产路径上多发 4 笔 CMD52。
        if retry % 500 == 0 && log::log_enabled!(log::Level::Trace) {
            let f1_bc = sdio.read_byte(1, SDIOWIFI_BLOCK_CNT_REG).unwrap_or(0xEE);
            let f2_bc = sdio.read_byte(2, SDIOWIFI_BLOCK_CNT_REG).unwrap_or(0xEE);
            let f1_fc = sdio.read_byte(1, SDIOWIFI_FLOW_CTRL_REG).unwrap_or(0xEE);
            let f0_ip = sdio.read_byte(0, 0x05).unwrap_or(0xEE);
            let irqc = IRQ_COUNT.load(Ordering::Relaxed);
            log::trace!(
                "[DIAG] retry={} f1_bc=0x{:02x} f2_bc=0x{:02x} f1_fc(0x0a)=0x{:02x} \
                 f0_intpend(0x05)=0x{:02x} irq#38={}",
                retry,
                f1_bc,
                f2_bc,
                f1_fc,
                f0_ip,
                irqc
            );
        }

        if raw & SDIO_OTHER_INTERRUPT != 0 {
            crate::runtime::runtime().sleep_ms(1);
            continue;
        }

        let block_cnt = raw & BLOCK_COUNT_MASK;
        if block_cnt == 0 {
            if retry > RESPONSE_MAX_RETRY - 1 {
                return Err("response timeout");
            }
            crate::runtime::runtime().sleep_ms(1);
            continue;
        }

        match read_and_parse_response(sdio, is_v3, func, block_cnt, expected_cfm, cfm_buf) {
            Ok(len) => return Ok(len),
            Err(e) => {
                log::warn!("[polling] {}", e);
                continue;
            }
        }
    }

    Err("response timeout")
}

/// 读取并解析响应帧
fn read_and_parse_response<H: SdioHost>(
    sdio: &H,
    is_v3: bool,
    func: u8,
    block_cnt: u8,
    expected_cfm: u16,
    cfm_buf: &mut [u8],
) -> Result<usize, &'static str> {
    let read_len = (block_cnt as usize) * SDIOWIFI_FUNC_BLOCKSIZE;
    let mut rx_buf: Vec<u8> = vec![0u8; read_len];

    if sdio
        .read_fifo(func, rd_fifo_reg(is_v3), &mut rx_buf)
        .is_err()
    {
        crate::runtime::runtime().sleep_ms(2);
        return Err("CRC error, retrying");
    }

    if read_len < PROTO_HEADER_SIZE - 4 {
        return Err("response too short");
    }

    let resp_id = u16::from_le_bytes([rx_buf[4], rx_buf[5]]);
    if resp_id != expected_cfm {
        log::error!(
            "unexpected resp_id=0x{:04x}, expected=0x{:04x}",
            resp_id,
            expected_cfm
        );
        return Err("unexpected response id");
    }

    let param_offset = PROTO_HEADER_SIZE; // = 16: SDIO(4) + LMAC(12, 含 pattern)
    let cfm_len = cfm_buf.len().min(read_len.saturating_sub(param_offset));
    if cfm_len > 0 {
        cfm_buf[..cfm_len].copy_from_slice(&rx_buf[param_offset..param_offset + cfm_len]);
    }

    Ok(cfm_len)
}

/// 轮询模式发送 LMAC 命令并等待 CFM
///
/// 用于 FDRV 初始化阶段（中断未使能），直接操作 SDIO 寄存器。
pub fn polling_send_cmd<H: SdioHost>(
    sdio: &H,
    is_v3: bool,
    func: u8,
    msg_id: u16,
    dest_id: u16,
    param: &[u8],
    wait_cfm: bool,
    cfm_buf: &mut [u8],
) -> Result<usize, &'static str> {
    let buf = build_polling_cmd_frame(msg_id, dest_id, param, is_v3);
    check_flow_control_polling(sdio, is_v3, func)?;
    log::debug!(
        "[fdrv] sending cmd 0x{:04x} via func{} fifo reg 0x{:02x}, len={}",
        msg_id,
        func,
        wr_fifo_reg(is_v3),
        buf.len()
    );

    if log::log_enabled!(log::Level::Trace) {
        let pre_f1 = sdio.read_byte(1, SDIOWIFI_BLOCK_CNT_REG).unwrap_or(0xEE);
        let pre_f2 = sdio.read_byte(2, SDIOWIFI_BLOCK_CNT_REG).unwrap_or(0xEE);
        log::trace!(
            "[fdrv] pre-send block_cnt f1=0x{:02x} f2=0x{:02x}",
            pre_f1,
            pre_f2
        );
    }

    sdio.write_fifo(func, wr_fifo_reg(is_v3), &buf)
        .map_err(|_| "write_fifo error")?;

    if !wait_cfm {
        return Ok(0);
    }

    let expected_cfm = msg_id + 1;
    poll_for_response(sdio, is_v3, func, expected_cfm, cfm_buf)
}

/// 排空残留数据
fn drain_stale_data<H: SdioHost>(sdio: &H, is_v3: bool) {
    for i in 0..10 {
        match sdio.read_byte(1, block_cnt_reg(is_v3)) {
            Ok(raw) => {
                if raw & SDIO_OTHER_INTERRUPT != 0 {
                    log::debug!("[fdrv] drain: SDIO_OTHER_INTERRUPT, raw=0x{:02x}", raw);
                    continue;
                }
                let block_cnt = raw & SDIOWIFI_FLOWCTRL_MASK;
                if block_cnt == 0 {
                    log::debug!("[fdrv] drain: no stale data (iteration {})", i);
                    break;
                }
                log::debug!(
                    "[fdrv] drain: block_cnt={}, reading and discarding",
                    block_cnt
                );
                let data_len = (block_cnt as usize) * SDIOWIFI_FUNC_BLOCKSIZE;
                let mut buf: Vec<u8> = vec![0u8; data_len];
                match sdio.read_fifo(1, rd_fifo_reg(is_v3), &mut buf) {
                    Ok(()) => log::debug!("[fdrv] drain: discarded {} bytes", data_len),
                    Err(e) => {
                        warn!(
                            "[fdrv] drain: read_fifo failed: {:?} (CRC error expected)",
                            e
                        );
                    }
                }
            }
            Err(e) => {
                warn!("[fdrv] drain: read_byte failed: {:?}", e);
                break;
            }
        }
    }
}

// ===== 初始化辅助函数 =====

/// 等待固件 SDIO 接口稳定
fn wait_for_firmware_stabilization() {
    log::debug!("[fdrv] waiting for firmware SDIO interface to stabilize...");
    crate::runtime::runtime().sleep_ms(200);
}

/// 排空初始化前的残留数据
fn drain_initial_stale_data<H: SdioHost>(sdio: &H, is_v3: bool, func: u8) {
    for i in 0..5u32 {
        match sdio.read_byte(func, block_cnt_reg(is_v3)) {
            Ok(raw) => {
                let cnt = raw & BLOCK_COUNT_MASK;
                if cnt == 0 {
                    log::debug!("[fdrv] drain: no stale data (iteration {})", i);
                    break;
                }
                let len = (cnt as usize) * SDIOWIFI_FUNC_BLOCKSIZE;
                let mut discard: Vec<u8> = vec![0u8; len];
                let _ = sdio.read_fifo(func, rd_fifo_reg(is_v3), &mut discard);
                log::debug!("[fdrv] drain: discarded {} bytes (block_cnt={})", len, cnt);
            }
            Err(e) => {
                warn!("[fdrv] drain: read_byte error: {:?}", e);
                break;
            }
        }
    }
}

/// 发送 MM_SET_STACK_START_REQ 命令
///
/// 对应 Linux rwnx_main.c:5822:
///   D80/D80X2: set_vendor_info = CO_BIT(5) = 0x20
///   其他芯片: set_vendor_info = 0
/// struct mm_set_stack_start_req: on(u8) + efuse_valid(u8) + set_vendor_info(u8) + fwtrace_redir(u8) = 4 bytes
fn send_stack_start_command<H: SdioHost>(
    sdio: &H,
    is_v3: bool,
    chip: ChipVariant,
) -> Result<(), &'static str> {
    let set_vendor_info: u8 = if matches!(chip, ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2)
    {
        0x20
    } else {
        0x00
    };
    let param: [u8; 4] = [0x01, 0x00, set_vendor_info, 0x00];

    let func = cmd_func(chip);
    let mut cfm = [0u8; 2];
    match polling_send_cmd(
        sdio,
        is_v3,
        func,
        MM_SET_STACK_START_REQ,
        TASK_MM,
        &param,
        true,
        &mut cfm,
    ) {
        Ok(len) => {
            log::debug!(
                "[fdrv] MM_SET_STACK_START_CFM OK, len={}, is_5g={}, vendor=0x{:02x}",
                len,
                if len > 0 { cfm[0] } else { 0 },
                if len > 1 { cfm[1] } else { 0 }
            );
            Ok(())
        }
        Err(e) => {
            error!("[fdrv] MM_SET_STACK_START_REQ failed: {}", e);
            Err("MM_SET_STACK_START_REQ failed")
        }
    }
}

/// 发送 LMAC 初始化命令（仅 stack_start）
///
/// reset 在 lmac_configure 中发送（vendor D80 序列：stack_start → rf_calib → get_mac → reset）
fn send_lmac_init_commands<H: SdioHost>(
    sdio: &H,
    is_v3: bool,
    chip: ChipVariant,
) -> Result<(), &'static str> {
    send_stack_start_command(sdio, is_v3, chip)?;
    Ok(())
}

/// 排空 LMAC 初始化后的残留数据
fn drain_post_init_data<H: SdioHost>(sdio: &H, is_v3: bool, func: u8) {
    crate::runtime::runtime().sleep_ms(50);

    for _ in 0..10u32 {
        match sdio.read_byte(func, block_cnt_reg(is_v3)) {
            Ok(raw) => {
                let cnt = raw & BLOCK_COUNT_MASK;
                if cnt == 0 {
                    break;
                }
                let len = (cnt as usize) * SDIOWIFI_FUNC_BLOCKSIZE;
                let mut discard: Vec<u8> = vec![0u8; len];
                let _ = sdio.read_fifo(func, rd_fifo_reg(is_v3), &mut discard);
                log::debug!("[fdrv] post-init drain: discarded {} bytes", len);
            }
            Err(_) => break,
        }
    }
}

/// 使能中断（SDHCI CARD_INT 和 AIC8800 芯片端）
fn enable_interrupts(bus: &WifiBus) -> Result<(), &'static str> {
    let transport = &bus.transport;

    // 使能 SDHCI CARD_INT 信号
    transport.enable_irq();

    // 使能 AIC8800 芯片端 SDIO 中断
    if transport
        .write_byte(1, transport.intr_config_reg_addr(), 0x07)
        .is_err()
    {
        return Err("intr_config_reg write failed");
    }

    // 验证 IRQ 触发
    crate::runtime::runtime().sleep_ms(2);
    let irq_cnt = IRQ_COUNT.load(Ordering::Relaxed);
    log::debug!("[VERIFY-1] IRQ#38 triggered {} times", irq_cnt);

    Ok(())
}

/// 启动 RX/TX 线程
fn start_driver_threads(bus: &alloc::sync::Arc<WifiBus>) {
    *bus.state.lock() = BusState::Up;
    rx::start(alloc::sync::Arc::clone(bus));
    tx::start(alloc::sync::Arc::clone(bus));
    ap::start(alloc::sync::Arc::clone(bus));
}

/// FDRV 初始化入口
///
/// 在 firmware_init 成功后调用。接受任意 SdioHost 实现。
/// 执行以下步骤：
/// 1. 等待固件 SDIO 接口稳定
/// 2. 排空残留数据
/// 3. 轮询模式发送 MM_SET_STACK_START_REQ（LMAC 初始化）
/// 4. 创建 SdioTransport + WifiBus
/// 5. 使能 CARD_INT 信号 + AIC8800 芯片端中断
/// 6. 启动 RX/TX 线程
///
/// 注意：PLIC IRQ 注册和 CARD_INT 回调注册由上层 (aic8800_wireless) 完成。
/// 固件启动后重新初始化 SDIO 功能寄存器
///
/// 参考 radxa FDRV probe 后的 aicwf_sdiov3_func_init:
/// V3: func0 0xF2=0x7F, 禁用 byte mode, wakeup=0x11, 检查 sleep_reg
fn reinit_sdio_func<H: SdioHost>(sdio: &mut H, chip: ChipVariant) -> Result<(), &'static str> {
    let is_v3 = chip.is_v3();
    let is_dc = matches!(chip, ChipVariant::Aic8800DC | ChipVariant::Aic8800DW);
    if is_v3 {
        sdio.write_byte(0, 0xF2, 0x7F)
            .map_err(|_| "func0 0xF2 write failed")?;
        sdio.write_byte(1, SDIOWIFI_BYTEMODE_ENABLE_REG_V3, 0x01)
            .map_err(|_| "bytemode disable failed")?;
        sdio.write_byte(1, SDIOWIFI_WAKEUP_REG_V3, SDIOWIFI_V3_WAKEUP_VALUE)
            .map_err(|_| "wakeup write failed")?;
        crate::runtime::runtime().sleep_ms(5);
        let sleep_val = sdio
            .read_byte(1, SDIOWIFI_SLEEP_REG_V3)
            .map_err(|_| "sleep_reg read failed")?;
        if sleep_val & SDIOWIFI_V3_SLEEP_READY_BIT == 0 {
            warn!(
                "[fdrv] V3 re-init wakeup not ready, sleep_reg=0x{:02x}",
                sleep_val
            );
        } else {
            log::debug!(
                "[fdrv] V3 re-init SDIO ready (sleep_reg=0x{:02x})",
                sleep_val
            );
        }
        sdio.write_byte(0, 0x04, 0x07)
            .map_err(|_| "func0 0x04 write failed")?;
        log::debug!("[fdrv] V3 FN0 reg 0x04 = 0x07 (interrupt enable)");
    } else if is_dc {
        // AIC8800DC/DW: 固件启动后, fdrv 阶段必须重做 SDIO func 初始化
        // (对齐 vendor aicwf_sdio_func_init)。固件 start 会重置 SDIO 状态机,
        // bootrom 阶段写的 block-mode 失效, 不重写则固件不按 block 模式回包,
        // block_cnt(0x12) 不递增 → 命令轮询永远超时。
        // 命令走 func1, 但 func2 也必须 enable + 设块大小 + 写 block/byte 模式。
        sdio.enable_func(2).map_err(|_| "func2 enable failed")?;
        sdio.set_block_size(2, SDIOWIFI_FUNC_BLOCKSIZE as u16)
            .map_err(|_| "func2 block size failed")?;
        sdio.write_byte(2, SDIOWIFI_REGISTER_BLOCK, 0x01)
            .map_err(|_| "func2 0x0B write failed")?;
        sdio.write_byte(2, SDIOWIFI_BYTEMODE_ENABLE_REG, 0x01)
            .map_err(|_| "func2 0x11 write failed")?;
        // func1: 块模式使能 + 禁字节模式
        sdio.write_byte(1, SDIOWIFI_REGISTER_BLOCK, 0x01)
            .map_err(|_| "func1 0x0B write failed")?;
        sdio.write_byte(1, SDIOWIFI_BYTEMODE_ENABLE_REG, 0x01)
            .map_err(|_| "func1 0x11 write failed")?;
        crate::runtime::runtime().sleep_ms(10);
        // 关键: 武装 func1 RX 中断路径 (对齐 vendor aicwf_sdio_bus_start)。
        // DC 固件的命令 CFM 经 SDIO 中断路径回包: 不写 INTR_CONFIG(0x04)=0x07,
        // 芯片永不驱动 RX, block_cnt(0x12) 恒 0 → 轮询永远超时。
        // 必须在发第一笔 MM 命令之前写, 否则第一笔命令拿不到响应。
        sdio.write_byte(1, intr_config_reg(is_v3), 0x07)
            .map_err(|_| "func1 0x04 intr_config write failed")?;
        log::info!("[fdrv] DC SDIO func re-init done (func1+func2 block mode, func1 intr armed)");
    }
    Ok(())
}

pub fn init<H: SdioHost + 'static>(
    mut sdio: H,
    chip: ChipVariant,
) -> Result<alloc::sync::Arc<WifiBus>, &'static str> {
    let is_v3 = chip.is_v3();
    let func = cmd_func(chip);

    // ---- Step 0: 等待固件 SDIO 接口稳定 ----
    wait_for_firmware_stabilization();

    // ---- Step 0.5: 固件启动后重新初始化 SDIO 功能寄存器 ----
    reinit_sdio_func(&mut sdio, chip)?;

    // ---- Step 1: 排空残留数据 ----
    drain_initial_stale_data(&sdio, is_v3, func);

    // ---- Step 2: 轮询模式发送 MM_SET_STACK_START_REQ ----
    send_lmac_init_commands(&sdio, is_v3, chip)?;

    // ---- Step 3: 排空 LMAC 初始化产生的残留数据 ----
    drain_post_init_data(&sdio, is_v3, func);

    // ---- Step 4: 创建 SdioTransport + WifiBus ----
    let transport = SdioTransport::new(sdio, chip);
    let bus = WifiBus::new(transport);
    set_global_bus(&bus);

    // ---- Step 5: 使能中断 ----
    enable_interrupts(&bus)?;

    // ---- Step 6: 启动线程 ----
    start_driver_threads(&bus);

    log::debug!("[fdrv] AIC8800 FDRV initialized");
    Ok(bus)
}
