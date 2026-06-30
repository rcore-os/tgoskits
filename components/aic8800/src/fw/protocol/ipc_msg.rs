//! AIC8800 IPC 消息协议
//!
//! 实现消息构建、SDIO 传输、响应解析。

use sdio_host::{SdioHost, error::SdioError};

use super::super::chip::{
    ChipVariant, DRV_TASK_ID, LMAC_FIRST_DBG, SDIO_TYPE_CFG_CMD_RSP, SDIOWIFI_BLOCK_CNT_REG,
    SDIOWIFI_FLOW_CTRL_Q1_REG_V3, SDIOWIFI_FLOW_CTRL_REG, SDIOWIFI_FLOWCTRL_MASK,
    SDIOWIFI_MISC_INT_STATUS_REG_V3, SDIOWIFI_RD_FIFO_ADDR, SDIOWIFI_RD_FIFO_ADDR_V3,
    SDIOWIFI_WR_FIFO_ADDR, SDIOWIFI_WR_FIFO_ADDR_V3, TASK_DBG,
};

// ============================================================
// LMAC 消息 ID 定义
// ============================================================

/// Debug 消息 ID
#[repr(u16)]
#[derive(Debug, Clone, Copy)]
pub enum DbgMsgId {
    MemReadReq,       // 0x0400
    MemReadCfm,       // 0x0401
    MemWriteReq,      // 0x0402
    MemWriteCfm,      // 0x0403
    SetModFilterReq,  // 0x0404
    SetModFilterCfm,  // 0x0405
    SetSevFilterReq,  // 0x0406
    SetSevFilterCfm,  // 0x0407
    ErrorInd,         // 0x0408
    GetSysStatReq,    // 0x0409
    GetSysStatCfm,    // 0x040a
    MemBlockWriteReq, // 0x040b
    MemBlockWriteCfm, // 0x040c
    StartAppReq,      // 0x040d
    StartAppCfm,      // 0x040e
    StartNpcReq,      // 0x040f
    StartNpcCfm,      // 0x0410
    MemMaskWriteReq,  // 0x0411
    MemMaskWriteCfm,  // 0x0412
}

impl DbgMsgId {
    pub fn msg_id(self) -> u16 {
        LMAC_FIRST_DBG
            + match self {
                Self::MemReadReq => 0,
                Self::MemReadCfm => 1,
                Self::MemWriteReq => 2,
                Self::MemWriteCfm => 3,
                Self::SetModFilterReq => 4,
                Self::SetModFilterCfm => 5,
                Self::SetSevFilterReq => 6,
                Self::SetSevFilterCfm => 7,
                Self::ErrorInd => 8,
                Self::GetSysStatReq => 9,
                Self::GetSysStatCfm => 10,
                Self::MemBlockWriteReq => 11,
                Self::MemBlockWriteCfm => 12,
                Self::StartAppReq => 13,
                Self::StartAppCfm => 14,
                Self::StartNpcReq => 15,
                Self::StartNpcCfm => 16,
                Self::MemMaskWriteReq => 17,
                Self::MemMaskWriteCfm => 18,
            }
    }
}

// ============================================================
// 消息缓冲区
// ============================================================
/// 最大消息大小: 8 (transport header) + 8 (lmac_msg header) + 1032 (block write payload)
const MSG_BUF_MAX: usize = 1536;

// ============================================================
// 协议常量（IPC 传输层私有）
// ============================================================
/// Transport header 大小 (字节)
const TRANSPORT_HEADER_SIZE: usize = 4;
/// Dummy word 大小 (字节)
const DUMMY_WORD_SIZE: usize = 4;
/// lmac_msg header 大小 (字节)
const LMAC_MSG_HEADER_SIZE: usize = 8;
/// 响应消息的 payload 偏移量
const RESPONSE_PAYLOAD_OFFSET: usize =
    TRANSPORT_HEADER_SIZE + DUMMY_WORD_SIZE + LMAC_MSG_HEADER_SIZE;
/// SDIO 块大小 (字节)
const SDIO_BLOCK_SIZE: usize = 512;

/// 4 字节对齐值
const TX_ALIGNMENT: usize = 4;

/// 尾部长度 (字节)
const TAIL_LEN: usize = 4;

// ============================================================
// 流控常量（IPC 传输层私有，仅固件上传阶段使用）
// ============================================================
/// 流控重试最大次数
const FLOW_CONTROL_MAX_RETRY: u32 = 50;

// ============================================================
// 响应等待常量
// ============================================================
/// SDIO_OTHER_INTERRUPT 标志位
const SDIO_OTHER_INTERRUPT_FLAG: u8 = 0x80;
/// 响应超时最大重试次数
const RESPONSE_MAX_RETRY: u32 = 100_000;
/// FIFO 读取错误最大重试次数
const FIFO_READ_MAX_RETRY: u32 = 5;

use crate::common::crc8_ponl_107;

/// IPC 消息传输层
///
/// 封装了消息的构建、发送和响应接收。
pub struct IpcTransport<'a, H: SdioHost> {
    sdio_host: &'a mut H,
    chip: ChipVariant,
    tx_buf: [u8; MSG_BUF_MAX],
    rx_buf: [u8; MSG_BUF_MAX],
}

impl<'a, H: SdioHost> IpcTransport<'a, H> {
    pub fn new(sdio_host: &'a mut H, chip: ChipVariant) -> Self {
        Self {
            sdio_host,
            chip,
            tx_buf: [0; MSG_BUF_MAX],
            rx_buf: [0; MSG_BUF_MAX],
        }
    }

    pub fn host(&mut self) -> &mut H {
        self.sdio_host
    }

    fn is_v3(&self) -> bool {
        matches!(
            self.chip,
            ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2
        )
    }

    /// AIC8800DC/DW 使用独立的 SDIO function 2 (func_msg) 作为命令邮箱
    fn is_dc(&self) -> bool {
        matches!(self.chip, ChipVariant::Aic8800DC | ChipVariant::Aic8800DW)
    }

    /// 命令通道使用的 SDIO function 号
    ///
    /// - AIC8800DC/DW: function 2 (func_msg) — bootrom 命令邮箱独立于数据口
    /// - AIC8801/D80/D80X2: function 1
    fn cmd_func(&self) -> u8 {
        if self.is_dc() { 2 } else { 1 }
    }

    fn flow_ctrl_reg(&self) -> u32 {
        if self.is_v3() {
            SDIOWIFI_FLOW_CTRL_Q1_REG_V3
        } else {
            SDIOWIFI_FLOW_CTRL_REG
        }
    }

    fn wr_fifo_addr(&self) -> u32 {
        if self.is_v3() {
            SDIOWIFI_WR_FIFO_ADDR_V3
        } else {
            SDIOWIFI_WR_FIFO_ADDR
        }
    }

    fn rd_fifo_addr(&self) -> u32 {
        if self.is_v3() {
            SDIOWIFI_RD_FIFO_ADDR_V3
        } else {
            SDIOWIFI_RD_FIFO_ADDR
        }
    }

    fn block_cnt_reg(&self) -> u32 {
        if self.is_v3() {
            SDIOWIFI_MISC_INT_STATUS_REG_V3
        } else {
            SDIOWIFI_BLOCK_CNT_REG
        }
    }

    /// 构建 Transport header (4 bytes)
    fn build_transport_header(&mut self, total_payload_len: usize) {
        self.tx_buf[0] = (total_payload_len & 0xFF) as u8; // 消息长度低字节
        self.tx_buf[1] = ((total_payload_len >> 8) & 0x0F) as u8; // 消息长度高字节
        self.tx_buf[2] = SDIO_TYPE_CFG_CMD_RSP; // 消息类型
        // byte[3]: AIC8800D80/D80X2 需要 CRC8, 其他为 0x00
        match self.chip {
            ChipVariant::Aic8800D80 | ChipVariant::Aic8800D80X2 => {
                self.tx_buf[3] = crc8_ponl_107(&self.tx_buf[0..3]);
            }
            _ => {
                self.tx_buf[3] = 0x00;
            }
        }
    }

    /// 填充 Dummy word (4 bytes)
    fn fill_dummy_word(&mut self) {
        self.tx_buf[4..8].fill(0);
    }

    /// 构建 lmac_msg header (8 bytes)
    fn build_lmac_msg_header(&mut self, msg_id: u16, payload_len: u16) {
        let idx = TRANSPORT_HEADER_SIZE + DUMMY_WORD_SIZE;
        self.tx_buf[idx..idx + 2].copy_from_slice(&msg_id.to_le_bytes()); // 消息 ID
        self.tx_buf[idx + 2..idx + 4].copy_from_slice(&TASK_DBG.to_le_bytes()); // dest_id
        self.tx_buf[idx + 4..idx + 6].copy_from_slice(&DRV_TASK_ID.to_le_bytes()); // src_id
        self.tx_buf[idx + 6..idx + 8].copy_from_slice(&payload_len.to_le_bytes()); // 消息负载长度
    }

    /// 复制 payload 到 tx_buf
    fn copy_payload(&mut self, payload: &[u8]) {
        let payload_start = TRANSPORT_HEADER_SIZE + DUMMY_WORD_SIZE + LMAC_MSG_HEADER_SIZE;
        self.tx_buf[payload_start..payload_start + payload.len()].copy_from_slice(payload);
    }

    /// 4 字节对齐
    fn align_to_4_bytes(&mut self, raw_len: usize) -> usize {
        let aligned4 = (raw_len + TX_ALIGNMENT - 1) & !(TX_ALIGNMENT - 1); // 向上对齐到 4 字节边界
        for i in raw_len..aligned4 {
            self.tx_buf[i] = 0; // 填充对齐字节为 0
        }
        aligned4
    }

    /// 块对齐 (512 字节)
    fn align_to_block(&mut self, aligned4: usize) -> usize {
        if !aligned4.is_multiple_of(SDIO_BLOCK_SIZE) {
            let with_tail = aligned4 + TAIL_LEN;
            let block_aligned = ((with_tail / SDIO_BLOCK_SIZE) + 1) * SDIO_BLOCK_SIZE; // 向上对齐到下一个 512 字节边界
            for i in aligned4..block_aligned.min(MSG_BUF_MAX) {
                self.tx_buf[i] = 0; // 填充对齐字节为 0
            }
            block_aligned
        } else {
            aligned4 // 已是 512 的整数倍, 不加 tail
        }
    }

    /// 构建 lmac_msg 头部 + transport header, 写入 tx_buf
    /// 返回总长度 (含 transport header)
    fn build_msg(&mut self, msg_id: u16, payload: &[u8]) -> usize {
        let lmac_msg_len = LMAC_MSG_HEADER_SIZE + payload.len(); //lmac_msg header + payload
        let total_payload_len = TRANSPORT_HEADER_SIZE + lmac_msg_len; // transport header + lmac_msg

        // 构建 Transport header
        self.build_transport_header(total_payload_len);

        // 填充 Dummy word
        self.fill_dummy_word();

        // 构建 lmac_msg header
        self.build_lmac_msg_header(msg_id, payload.len() as u16);

        // 复制 payload
        self.copy_payload(payload);

        // raw_len = transport header (4) + dummy (4) + lmac_msg header (8) + param
        let raw_len = TRANSPORT_HEADER_SIZE + DUMMY_WORD_SIZE + lmac_msg_len;

        // Step 1: 4 字节对齐 (TX_ALIGNMENT)
        let aligned4 = self.align_to_4_bytes(raw_len);

        // Step 2: 块对齐 — 仅当不是 512 倍数时加 TAIL_LEN(4)
        self.align_to_block(aligned4)
    }

    /// 等待流控允许发送
    fn wait_flow_control(&mut self) -> Result<(), SdioError> {
        let mut fc_retry = 0u32;
        loop {
            let fc_reg = self.sdio_host.read_byte(1, self.flow_ctrl_reg())?;
            if fc_reg & SDIOWIFI_FLOWCTRL_MASK != 0 {
                return Ok(());
            }
            fc_retry += 1;
            if fc_retry > FLOW_CONTROL_MAX_RETRY {
                log::error!("IPC: flow control timeout, last fc_reg=0x{:02x}", fc_reg);
                return Err(SdioError::Timeout);
            }
            crate::runtime::runtime().sleep_ms(1);
        }
    }

    /// 写入消息到 FIFO
    fn write_to_fifo(&mut self, send_len: usize) -> Result<(), SdioError> {
        let func = self.cmd_func();
        let addr = self.wr_fifo_addr();
        self.sdio_host
            .write_fifo(func, addr, &self.tx_buf[..send_len])?;
        Ok(())
    }

    /// 等待 SDIO 接口稳定 (Linux: udelay(200)~mdelay(2))
    fn wait_sdio_stable(&self) {
        crate::runtime::runtime().sleep_ms(2);
    }

    /// 处理 FIFO 读取错误
    fn handle_fifo_read_error(&mut self, read_err_cnt: u32, msg_id: u16) -> Result<(), SdioError> {
        log::warn!(
            "IPC: read_fifo error ({}/{}) for msg_id=0x{:04x}",
            read_err_cnt,
            FIFO_READ_MAX_RETRY,
            msg_id
        );
        if read_err_cnt > FIFO_READ_MAX_RETRY {
            log::error!("IPC: too many read errors for msg_id=0x{:04x}", msg_id);
            return Err(SdioError::CrcError);
        }
        // 等待芯片 SDIO 接口稳定 (DAT 线已在 sdhci 层复位)
        self.wait_sdio_stable();
        Ok(())
    }

    /// 轮询 BLOCK_CNT_REG 等待响应数据
    fn poll_block_count(&mut self) -> Result<Option<usize>, SdioError> {
        let func = self.cmd_func();
        let raw_cnt = self.sdio_host.read_byte(func, self.block_cnt_reg())?;
        // mask 掉 SDIO_OTHER_INTERRUPT (bit7)
        if raw_cnt & SDIO_OTHER_INTERRUPT_FLAG != 0 {
            log::warn!("IPC: SDIO_OTHER_INTERRUPT set, raw_cnt=0x{:02x}", raw_cnt);
            return Ok(None);
        } else if raw_cnt > 0 {
            let block_cnt = raw_cnt as usize;
            let read_len = (block_cnt * SDIO_BLOCK_SIZE).min(MSG_BUF_MAX);
            return Ok(Some(read_len));
        }
        Ok(None)
    }

    /// 从 FIFO 读取响应
    fn read_response_fifo(&mut self, read_len: usize) -> Result<(), SdioError> {
        let func = self.cmd_func();
        let addr = self.rd_fifo_addr();
        self.sdio_host
            .read_fifo(func, addr, &mut self.rx_buf[..read_len])?;
        Ok(())
    }

    /// 验证响应消息 ID
    fn validate_response_id(&self, expected_id: u16) -> Result<(), SdioError> {
        let resp_id = u16::from_le_bytes([self.rx_buf[4], self.rx_buf[5]]);
        // lmac_msg header 中的 msg_id 位于 offset 8-9
        if resp_id != expected_id {
            log::error!(
                "IPC: unexpected response id=0x{:04x}, expected=0x{:04x}",
                resp_id,
                expected_id
            );
            return Err(SdioError::CrcError);
        }
        Ok(())
    }

    /// 提取响应负载
    fn extract_response_payload(&self, cfm_buf: &mut [u8], read_len: usize) -> usize {
        let payload_offset = RESPONSE_PAYLOAD_OFFSET;
        let cfm_len = cfm_buf.len().min(read_len.saturating_sub(payload_offset));
        if cfm_len > 0 {
            cfm_buf[..cfm_len]
                .copy_from_slice(&self.rx_buf[payload_offset..payload_offset + cfm_len]);
        }
        cfm_len
    }

    /// 等待响应消息
    fn wait_for_response(&mut self, msg_id: u16, cfm_buf: &mut [u8]) -> Result<usize, SdioError> {
        self.wait_for_response_to(msg_id, cfm_buf, RESPONSE_MAX_RETRY)
    }

    /// 等待响应消息 (可指定最大重试次数, 用于短超时探测)
    fn wait_for_response_to(
        &mut self,
        msg_id: u16,
        cfm_buf: &mut [u8],
        max_retry: u32,
    ) -> Result<usize, SdioError> {
        let mut retry = 0u32;
        let mut read_err_cnt = 0u32;
        let expected_id = msg_id + 1;

        loop {
            match self.poll_block_count() {
                Ok(Some(read_len)) => match self.read_response_fifo(read_len) {
                    Ok(()) => {
                        self.validate_response_id(expected_id)?;
                        let cfm_len = self.extract_response_payload(cfm_buf, read_len);
                        return Ok(cfm_len);
                    }
                    Err(e) => {
                        log::warn!("IPC: read_response_fifo error: {:?}", e);
                        read_err_cnt += 1;
                        self.handle_fifo_read_error(read_err_cnt, msg_id)?;
                        continue;
                    }
                },
                Ok(None) => {
                    retry += 1;
                    if retry > max_retry {
                        return Err(SdioError::Timeout);
                    }
                    crate::runtime::runtime().sleep_ms(1);
                }
                Err(e) => {
                    log::warn!("IPC: poll_block_count error: {:?}", e);
                    return Err(e);
                }
            }
        }
    }

    /// 发送 IPC 消息并等待响应
    pub fn send_msg(
        &mut self,
        msg_id: DbgMsgId,
        payload: &[u8],
        wait_cfm: bool,
        cfm_buf: &mut [u8],
    ) -> Result<usize, SdioError> {
        let id = msg_id.msg_id();
        let send_len = self.build_msg(id, payload);

        // AIC8800DC/DW 的命令路径不经过流控寄存器 (0x0A);
        // 流控仅用于后续批量 data TX。直接写 func2 FIFO。
        if !self.is_dc() {
            self.wait_flow_control()?;
        }
        self.write_to_fifo(send_len)?;

        if !wait_cfm {
            return Ok(0);
        }

        self.wait_for_response(id, cfm_buf)
    }

    /// 发送消息并以短超时等待响应 (探测用, max_retry ms 后返回 Timeout 而非死等)
    pub fn send_msg_short(
        &mut self,
        msg_id: DbgMsgId,
        payload: &[u8],
        cfm_buf: &mut [u8],
        max_retry: u32,
    ) -> Result<usize, SdioError> {
        let id = msg_id.msg_id();
        let send_len = self.build_msg(id, payload);
        if !self.is_dc() {
            self.wait_flow_control()?;
        }
        self.write_to_fifo(send_len)?;
        self.wait_for_response_to(id, cfm_buf, max_retry)
    }
}

// ============================================================
// 高层消息接口
// ============================================================

/// 读取芯片内存 (4 字节)
pub fn ipc_mem_read<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    addr: u32,
) -> Result<u32, SdioError> {
    let payload = addr.to_le_bytes(); // 4 字节地址作为消息负载
    let mut cfm = [0u8; 8];
    transport.send_msg(DbgMsgId::MemReadReq, &payload, true, &mut cfm)?;
    let data = u32::from_le_bytes([cfm[4], cfm[5], cfm[6], cfm[7]]); // cfm[0..4]=memaddr, cfm[4..8]=memdata
    Ok(data)
}

/// 写入芯片内存 (4 字节)
pub fn ipc_mem_write<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    addr: u32,
    data: u32,
) -> Result<(), SdioError> {
    // param: memaddr (4) + memdata (4)
    let mut payload = [0u8; 8];
    payload[..4].copy_from_slice(&addr.to_le_bytes()); // 前 4 字节为地址
    payload[4..].copy_from_slice(&data.to_le_bytes()); // 后 4 字节为数据
    let mut cfm = [0u8; 8];
    transport.send_msg(DbgMsgId::MemWriteReq, &payload, true, &mut cfm)?;
    Ok(())
}

/// 探测写: 短超时 (2s) 写 4 字节, 超时返回 Err 而非死等 100s。
/// 用于诊断哪些地址可写而不被首个 hang 卡死。
pub fn ipc_mem_write_probe<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    addr: u32,
    data: u32,
) -> Result<(), SdioError> {
    let mut payload = [0u8; 8];
    payload[..4].copy_from_slice(&addr.to_le_bytes());
    payload[4..].copy_from_slice(&data.to_le_bytes());
    let mut cfm = [0u8; 8];
    transport.send_msg_short(DbgMsgId::MemWriteReq, &payload, &mut cfm, 2000)?;
    Ok(())
}

/// 块写入芯片内存 (最大 1032 字节)
pub fn ipc_mem_block_write<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    addr: u32,
    data: &[u8],
) -> Result<(), SdioError> {
    assert!(data.len() <= 1024, "block write max 1024 bytes");
    // param: memaddr (4) + memsize (4) + memdata[256] (up to 1024 bytes)
    let payload_len = 4 + 4 + data.len(); // 地址 + 大小 + 数据
    // 构建 param 到栈上缓冲区
    let mut payload = [0u8; 1032]; // 4 + 4 + 1024
    payload[..4].copy_from_slice(&addr.to_le_bytes()); // 前 4 字节为地址
    payload[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes()); // 后 4 字节为大小
    payload[8..8 + data.len()].copy_from_slice(data); // 后续字节为数据
    let mut cfm = [0u8; 4];
    transport.send_msg(
        DbgMsgId::MemBlockWriteReq,
        &payload[..payload_len],
        true,
        &mut cfm,
    )?;
    Ok(())
}

/// 掩码写入芯片内存 (DBG_MEM_MASK_WRITE_REQ = 0x0411)
pub fn ipc_mem_mask_write<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    addr: u32,
    mask: u32,
    data: u32,
) -> Result<(), SdioError> {
    // param: memaddr(4) + memmask(4) + memdata(4) = 12 bytes
    let mut payload = [0u8; 12];
    payload[0..4].copy_from_slice(&addr.to_le_bytes());
    payload[4..8].copy_from_slice(&mask.to_le_bytes());
    payload[8..12].copy_from_slice(&data.to_le_bytes());
    // cfm: memaddr(4) + memdata(4) = 8 bytes
    let mut cfm = [0u8; 8];
    transport.send_msg(DbgMsgId::MemMaskWriteReq, &payload, true, &mut cfm)?;
    Ok(())
}

/// 启动固件
pub fn ipc_start_app<H: SdioHost>(
    transport: &mut IpcTransport<H>,
    boot_addr: u32,
    boot_type: u32,
) -> Result<u32, SdioError> {
    // param: bootaddr (4) + boottype (4)
    let mut payload = [0u8; 8];
    payload[..4].copy_from_slice(&boot_addr.to_le_bytes()); // 前 4 字节为启动地址
    payload[4..].copy_from_slice(&boot_type.to_le_bytes()); // 后 4 字节为启动类型
    let mut cfm = [0u8; 4];
    transport.send_msg(DbgMsgId::StartAppReq, &payload, true, &mut cfm)?;
    let boot_status = u32::from_le_bytes([cfm[0], cfm[1], cfm[2], cfm[3]]);
    Ok(boot_status)
}
