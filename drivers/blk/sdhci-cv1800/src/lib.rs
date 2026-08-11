//! CVI SoC (CV1800/SG2002) SDHCI 控制器驱动
//!
//! 职责:
//!   - SDHCI 标准寄存器操作 (CMD52/CMD53/PIO)
//!   - SDIO 卡枚举 (CMD5/CMD3/CMD7)
//!   - 中断处理 (ISR 仅 CARD_INT, PIO 事件直接轮询 INT_STATUS)
//!   - 时钟/电源/总线宽度配置
//!
//! 设计:
//!   - ISR: 仅处理 CARD_INT (WiFi 芯片通知有数据可读)
//!   - PIO: wait_* 方法直接轮询 INT_STATUS 寄存器，W1C 清除
//!   - 分离 ISR/PIO 消除竞态条件

#![no_std]

extern crate alloc;

pub mod hw_init;
pub mod irq;
pub mod regs;
pub mod runtime;

use alloc::sync::Arc;
use core::ptr::{read_volatile, write_volatile};

pub use runtime::{SdhciDelay, set_delay};
use sdio_host::{SdioCardIrq, SdioHost, cccr::*, cmd::*, error::SdioError};

use crate::regs::*;

#[inline]
pub(crate) fn delay_ms(ms: u64) {
    crate::runtime::delay().delay_ms(ms);
}

pub(crate) fn mmio_read<T: Copy>(addr: usize) -> T {
    unsafe { read_volatile(addr as *const T) }
}

pub(crate) fn mmio_write<T: Copy>(addr: usize, val: T) {
    unsafe { write_volatile(addr as *mut T, val) }
}

pub struct CviCardIrqCtrl {
    base: usize,
}

impl CviCardIrqCtrl {
    pub fn new(base: usize) -> Self {
        Self { base }
    }
}

impl SdioCardIrq for CviCardIrqCtrl {
    fn mask_card_irq(&self) {
        irq::mask_card_irq_raw(self.base, true);
    }

    fn unmask_card_irq(&self) {
        irq::mask_card_irq_raw(self.base, false);
    }
}

/// CVI SoC WiFi SDIO 控制器
pub struct CviSdhci {
    base: usize, // MMIO 基地址
    rca: u16,    // 相对卡地址
    vendor_id: u16,
    device_id: u16,
}

impl CviSdhci {
    pub fn new(base_addr: usize) -> Self {
        Self {
            base: base_addr,
            rca: 0,
            vendor_id: 0,
            device_id: 0,
        }
    }

    #[inline(always)]
    fn read<T: Copy>(&self, off: u32) -> T {
        mmio_read::<T>(self.base + off as usize)
    }
    #[inline(always)]
    fn write<T: Copy>(&self, off: u32, val: T) {
        mmio_write::<T>(self.base + off as usize, val)
    }

    fn classify_error(err: u16) -> SdioError {
        if err & ERR_INT_CMD_CRC != 0 {
            log::error!("[SDHCI] CMD CRC error (err_sts=0x{:04x})", err);
        }
        if err & ERR_INT_DAT_CRC != 0 {
            log::error!("[SDHCI] DAT CRC error (err_sts=0x{:04x})", err);
        }
        if err & ERR_INT_CMD_TIMEOUT != 0 {
            log::error!("[SDHCI] CMD timeout (err_sts=0x{:04x})", err);
        }
        if err & ERR_INT_DAT_TIMEOUT != 0 {
            log::error!("[SDHCI] DAT timeout (err_sts=0x{:04x})", err);
        }
        match err {
            e if e & (ERR_INT_CMD_CRC | ERR_INT_DAT_CRC) != 0 => SdioError::CrcError,
            e if e & (ERR_INT_CMD_TIMEOUT | ERR_INT_DAT_TIMEOUT) != 0 => SdioError::Timeout,
            _ => SdioError::IoError,
        }
    }

    /// 直接轮询 INT_STATUS_NORM，等待指定 bit 置位后 W1C 清除
    ///
    /// 同时检测 Error 中断：如果 ERROR bit (bit 15) 置位，
    /// 读取 ERR_STATUS 并 W1C 清除所有状态位，然后返回错误。
    fn poll_int_status(&self, bit: u16) -> Result<(), SdioError> {
        // Phase 1: 快速自旋
        for _ in 0..1000 {
            let norm = self.read::<u16>(SDHCI_INT_STATUS_NORM);
            if norm & NORM_INT_ERROR != 0 {
                let err = self.read::<u16>(SDHCI_INT_STATUS_ERR);
                self.write::<u16>(SDHCI_INT_STATUS_ERR, err);
                self.write::<u16>(SDHCI_INT_STATUS_NORM, norm);
                self.reset_dat_line();
                return Err(Self::classify_error(err));
            }
            if norm & bit != 0 {
                self.write::<u16>(SDHCI_INT_STATUS_NORM, bit);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        // Phase 2: 协作式等待
        for i in 0..200_000 {
            let norm = self.read::<u16>(SDHCI_INT_STATUS_NORM);
            if norm & NORM_INT_ERROR != 0 {
                let err = self.read::<u16>(SDHCI_INT_STATUS_ERR);
                self.write::<u16>(SDHCI_INT_STATUS_ERR, err);
                self.write::<u16>(SDHCI_INT_STATUS_NORM, norm);
                self.reset_dat_line();
                return Err(Self::classify_error(err));
            }
            if norm & bit != 0 {
                self.write::<u16>(SDHCI_INT_STATUS_NORM, bit);
                return Ok(());
            }
            if i == 100_000 {
                let pres = self.read::<u32>(SDHCI_PRESENT_STATE);
                log::warn!(
                    "[SDHCI] poll_int mid-timeout: bit=0x{:04x} PRES=0x{:08x} INT_STS=0x{:04x}",
                    bit,
                    pres,
                    norm
                );
            }
            crate::runtime::delay().yield_now();
        }
        let pres = self.read::<u32>(SDHCI_PRESENT_STATE);
        let sts = self.read::<u16>(SDHCI_INT_STATUS_NORM);
        log::error!(
            "[SDHCI] poll_int_status timeout: bit=0x{:04x} PRES=0x{:08x} INT_STS=0x{:04x}",
            bit,
            pres,
            sts
        );
        // 超时后总线可能仍处于 DAT-busy(PRES bit1 DATA_INHIBIT 置位),若不复位
        // DAT 线状态机,后续任何数据命令的 wait_data_idle 都会一直超时,整条 SDIO
        // 总线被焊死(连 WiFi 模式切回 AP 也起不来)。这里对齐错误中断分支:清残留
        // INT_STATUS 并复位 DAT 线,让总线能从一次读超时中恢复。
        self.write::<u16>(SDHCI_INT_STATUS_NORM, sts);
        self.reset_dat_line();
        Err(SdioError::Timeout)
    }

    fn wait_cmd_complete(&self) -> Result<u32, SdioError> {
        self.poll_int_status(NORM_INT_CMD_COMPLETE)?;
        Ok(self.read::<u32>(SDHCI_RESPONSE))
    }

    fn wait_buffer_read_ready(&self) -> Result<(), SdioError> {
        self.poll_int_status(NORM_INT_BUF_RD_READY)
    }

    fn wait_buffer_write_ready(&self) -> Result<(), SdioError> {
        self.poll_int_status(NORM_INT_BUF_WR_READY)
    }

    fn wait_transfer_complete(&self) -> Result<(), SdioError> {
        self.poll_int_status(NORM_INT_XFER_COMPLETE)
    }

    /// 等待 CMD 线空闲 (仅检查 CMD_INHIBIT)
    fn wait_cmd_idle(&self) -> Result<(), SdioError> {
        for _ in 0..CMD_RESPONSE_TIMEOUT {
            if self.read::<u32>(SDHCI_PRESENT_STATE) & SDHCI_CMD_INHIBIT == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(SdioError::Timeout)
    }

    /// 等待 CMD 和 DAT 线都空闲 (数据命令前使用)
    fn wait_data_idle(&self) -> Result<(), SdioError> {
        for _ in 0..CMD_RESPONSE_TIMEOUT {
            if self.read::<u32>(SDHCI_PRESENT_STATE) & (SDHCI_CMD_INHIBIT | SDHCI_DATA_INHIBIT) == 0
            {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        let pres = self.read::<u32>(SDHCI_PRESENT_STATE);
        log::error!("[SDHCI] wait_data_idle timeout: PRES=0x{:08x}", pres);
        Err(SdioError::Timeout)
    }

    fn wait_clock_stable(&self) -> Result<(), SdioError> {
        for _ in 0..CLOCK_STABLE_TIMEOUT {
            if self.read::<u16>(SDHCI_CLOCK_CONTROL) & CC_INT_CLK_STABLE != 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(SdioError::Timeout)
    }

    fn wait_reset_complete(&self) -> Result<(), SdioError> {
        for _ in 0..RESET_TIMEOUT {
            if self.read::<u8>(SDHCI_SOFTWARE_RESET) == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(SdioError::Timeout)
    }

    fn reset_dat_line(&self) {
        self.write::<u8>(SDHCI_SOFTWARE_RESET, SWRST_DAT_LINE);
        for _ in 0..RESET_TIMEOUT {
            if self.read::<u8>(SDHCI_SOFTWARE_RESET) & SWRST_DAT_LINE == 0 {
                return;
            }
            core::hint::spin_loop();
        }
    }

    /// Clear stale INT_STATUS bits (W1C clear all set bits)
    fn clear_stale_status(&self) {
        let norm = self.read::<u16>(SDHCI_INT_STATUS_NORM);
        if norm != 0 {
            if norm & NORM_INT_ERROR != 0 {
                let err = self.read::<u16>(SDHCI_INT_STATUS_ERR);
                if err != 0 {
                    self.write::<u16>(SDHCI_INT_STATUS_ERR, err);
                }
            }
            self.write::<u16>(SDHCI_INT_STATUS_NORM, norm);
        }
    }

    /// Clear DAT state machine and stale INT_STATUS for first data transfer.
    pub fn prepare_first_data_xfer(&self) {
        self.write::<u16>(SDHCI_INT_STATUS_NORM, 0xFFFF);
        self.write::<u16>(SDHCI_INT_STATUS_ERR, 0xFFFF);
        self.reset_dat_line();
        log::debug!("[SDHCI] DAT line reset + INT_STATUS cleared for first data xfer");
    }

    /// SD 命令 (非数据命令: CMD0/3/5/7/52)
    fn send_cmd(&self, cmd_idx: u8, arg: u32) -> Result<u32, SdioError> {
        self.wait_cmd_idle()?;
        self.clear_stale_status();

        self.write::<u32>(SDHCI_ARGUMENT, arg);
        let flags = match cmd_idx {
            0 => CMD_RESP_NONE,
            3 => CMD_FLAGS_R5, // R6 与 R5 标志相同
            5 => CMD_FLAGS_R4,
            7 => CMD_FLAGS_R1B,
            52 => CMD_FLAGS_R5,
            _ => return Err(SdioError::Unsupported),
        };

        self.write::<u16>(
            SDHCI_COMMAND,
            (cmd_idx as u16) << CMD_INDEX_SHIFT as u16 | flags,
        );
        self.wait_cmd_complete()
    }

    fn check_r5_response(&self, resp: u32) -> Result<u8, SdioError> {
        if resp & R5_COM_CRC_ERROR != 0 {
            log::error!("[SDHCI] R5 CRC error, resp=0x{:08x}", resp);
            return Err(SdioError::CrcError);
        }
        if resp & (R5_ILLEGAL_COMMAND | R5_FUNCTION_NUMBER | R5_OUT_OF_RANGE) != 0 {
            log::error!("[SDHCI] R5 cmd/func/range error, resp=0x{:08x}", resp);
            return Err(SdioError::IoError);
        }
        if resp & R5_ERROR != 0 {
            log::error!("[SDHCI] R5 general error, resp=0x{:08x}", resp);
            return Err(SdioError::IoError);
        }
        Ok((resp & R5_DATA_MASK) as u8)
    }

    /// CMD52
    fn cmd52(&self, func: u8, addr: u32, flags: u32, val: u8) -> Result<u8, SdioError> {
        if addr > SDIO_ADDR_MASK {
            return Err(SdioError::Unsupported);
        }
        let arg =
            flags | ((func as u32 & 0x07) << 28) | ((addr & SDIO_ADDR_MASK) << 9) | val as u32;
        let resp = self.send_cmd(52, arg)?;
        self.check_r5_response(resp)
    }

    fn cmd52_read(&self, func: u8, addr: u32) -> Result<u8, SdioError> {
        self.cmd52(func, addr, 0, 0)
    }

    fn cmd52_write(&self, func: u8, addr: u32, val: u8) -> Result<(), SdioError> {
        self.cmd52(func, addr, CMD52_RW_FLAG, val)?;
        Ok(())
    }

    /// CMD53 数据传输设置
    ///
    /// 关键改进:
    ///   - 检查 DATA_INHIBIT (确保前一次数据传输完成)
    ///   - TRANSFER_MODE + COMMAND 作为 32-bit 原子写入
    ///   - BLOCK_SIZE 寄存器设置 SDMA boundary 字段
    #[allow(clippy::too_many_arguments)]
    fn cmd53_xfer(
        &self,
        func: u8,
        addr: u32,
        write: bool,
        inc_addr: bool,
        block_size: u16,
        use_block: bool,
        len: usize,
    ) -> Result<(u16, u16), SdioError> {
        if addr > SDIO_ADDR_MASK || len == 0 {
            return Err(SdioError::Unsupported);
        }

        let (blk_mode, count, blk_sz) = if use_block && block_size > 0 {
            let n = len / block_size as usize;
            if n == 0 || !len.is_multiple_of(block_size as usize) {
                return Err(SdioError::Unsupported);
            }
            (true, n, block_size)
        } else {
            if len > SDIO_DEFAULT_BLOCK_SIZE as usize {
                return Err(SdioError::Unsupported);
            }
            (
                false,
                if len == SDIO_DEFAULT_BLOCK_SIZE as usize {
                    0
                } else {
                    len
                },
                len as u16,
            )
        };

        let mut arg =
            ((func as u32 & 0x07) << 28) | ((addr & SDIO_ADDR_MASK) << 9) | (count as u32 & 0x1FF);
        if write {
            arg |= CMD53_RW_FLAG;
        }
        if blk_mode {
            arg |= CMD53_BLOCK_MODE;
        }
        if inc_addr {
            arg |= CMD53_OP_CODE_INC;
        }

        let xfer_blocks = if blk_mode { count as u16 } else { 1 };

        // 等待 CMD 和 DAT 线都空闲
        self.wait_data_idle()?;
        self.clear_stale_status();

        // BLOCK_SIZE: bits[11:0]=block size, bits[14:12]=SDMA boundary (0x7=512K)
        self.write::<u16>(SDHCI_BLOCK_SIZE, blk_sz | SDHCI_SDMA_BOUNDARY_512K);
        self.write::<u16>(SDHCI_BLOCK_COUNT, xfer_blocks);

        // TRANSFER_MODE (offset 0x0C) + COMMAND (offset 0x0E) 作为 32-bit 原子写入
        let tm = if blk_mode {
            TM_MULTI_BLOCK | TM_BLK_CNT_EN
        } else {
            0
        } | if !write { TM_DATA_DIR_READ } else { 0 };

        let cmd_val = (53u16) << CMD_INDEX_SHIFT as u16 | CMD_FLAGS_R5_DATA;
        self.write::<u32>(SDHCI_ARGUMENT, arg);
        self.write::<u32>(SDHCI_TRANSFER_MODE, ((cmd_val as u32) << 16) | (tm as u32));

        self.wait_cmd_complete()?;
        Ok((blk_sz, xfer_blocks))
    }

    fn cmd53_read_fixed(
        &self,
        func: u8,
        addr: u32,
        buf: &mut [u8],
        blk_sz: u16,
        use_blk: bool,
    ) -> Result<(), SdioError> {
        let (bs, nb) = self.cmd53_xfer(func, addr, false, false, blk_sz, use_blk, buf.len())?;
        self.pio_read(buf, bs, nb)?;
        self.wait_transfer_complete()
    }

    fn cmd53_write_fixed(
        &self,
        func: u8,
        addr: u32,
        buf: &[u8],
        blk_sz: u16,
        use_blk: bool,
    ) -> Result<(), SdioError> {
        let (bs, nb) = self.cmd53_xfer(func, addr, true, false, blk_sz, use_blk, buf.len())?;
        self.pio_write(buf, bs, nb)?;
        self.wait_transfer_complete()
    }

    /// PIO 读取: 逐块等待 Buffer Read Ready → 读取 Buffer Data Port
    fn pio_read(&self, buf: &mut [u8], block_size: u16, nblocks: u16) -> Result<(), SdioError> {
        let mut offset = 0;

        for _ in 0..nblocks {
            self.wait_buffer_read_ready()?;

            let words = (block_size as usize).div_ceil(4);
            for _ in 0..words {
                let data = self.read::<u32>(SDHCI_BUFFER);
                let byte_offset = data.to_le_bytes();
                let remaining = buf.len() - offset;
                let copy_len = core::cmp::min(4, remaining);
                buf[offset..offset + copy_len].copy_from_slice(&byte_offset[..copy_len]);
                offset += copy_len;
            }
        }

        Ok(())
    }

    /// PIO 写入: 逐块等待 Buffer Write Ready → 写入 Buffer Data Port
    fn pio_write(&self, buf: &[u8], block_size: u16, nblocks: u16) -> Result<(), SdioError> {
        let mut offset = 0;

        for _ in 0..nblocks {
            self.wait_buffer_write_ready()?;

            let words = (block_size as usize).div_ceil(4);
            for _ in 0..words {
                let mut data: [u8; 4] = [0; 4];
                let remaining = buf.len() - offset;
                let copy_len = core::cmp::min(4, remaining);
                data[..copy_len].copy_from_slice(&buf[offset..offset + copy_len]);
                let word = u32::from_le_bytes(data);
                self.write::<u32>(SDHCI_BUFFER, word);
                offset += copy_len;
            }
        }

        Ok(())
    }

    /// 读取 CIS 指针 (3 字节, little-endian)
    fn read_cis_ptr(&self, func: u8) -> Result<u32, SdioError> {
        let base = if func == 0 {
            CCCR_CIS_POINTER
        } else {
            fbr_base(func) + FBR_CIS_PTR_OFFSET
        };
        let b0 = self.cmd52_read(0, base)? as u32;
        let b1 = self.cmd52_read(0, base + 1)? as u32;
        let b2 = self.cmd52_read(0, base + 2)? as u32;
        Ok(b0 | (b1 << 8) | (b2 << 16))
    }

    /// 遍历 CIS tuple 链，查找 CISTPL_MANFID，返回 (vendor_id, device_id)
    fn read_manfid_from_cis(&self, func: u8) -> Result<(u16, u16), SdioError> {
        let mut addr = self.read_cis_ptr(func)?;
        for _ in 0..256 {
            let tuple_code = self.cmd52_read(0, addr)?;
            if tuple_code == CISTPL_END {
                break;
            }
            if tuple_code == CISTPL_NULL {
                addr += 1;
                continue;
            }
            let tuple_link = self.cmd52_read(0, addr + 1)? as u32;
            if tuple_code == CISTPL_MANFID && tuple_link >= 4 {
                let v0 = self.cmd52_read(0, addr + 2)? as u16;
                let v1 = self.cmd52_read(0, addr + 3)? as u16;
                let v2 = self.cmd52_read(0, addr + 4)? as u16;
                let v3 = self.cmd52_read(0, addr + 5)? as u16;
                return Ok((v0 | (v1 << 8), v2 | (v3 << 8)));
            }
            addr += 2 + tuple_link;
        }

        Err(SdioError::Unsupported)
    }

    // ========== SDIO 初始化辅助函数 ==========

    /// SDHCI 控制器软件复位
    fn controller_reset(&self) -> Result<(), SdioError> {
        self.write::<u8>(SDHCI_SOFTWARE_RESET, SWRST_ALL);
        self.wait_reset_complete()
    }

    /// 设置卡检测覆写（WiFi 模块无物理 CD 引脚）
    fn setup_card_detect(&self) -> Result<(), SdioError> {
        let hc = self.read::<u8>(SDHCI_HOST_CONTROL);
        self.write::<u8>(SDHCI_HOST_CONTROL, hc | HC_CARD_DET_TEST | HC_CARD_DET_SEL);
        Ok(())
    }

    /// 上电 3.3V（必须在启动时钟之前）
    fn power_on(&self) -> Result<(), SdioError> {
        self.write::<u8>(SDHCI_POWER_CONTROL, POWER_330V_ON);
        Ok(())
    }

    /// 设置初始低速时钟 400KHz
    fn setup_initial_clock(&self) -> Result<(), SdioError> {
        self.set_clock(400_000)
    }

    /// 使能中断状态位 + CARD_INT 信号
    fn enable_interrupts_irq(&self) -> Result<(), SdioError> {
        irq::irq_state_init(self.base);
        // Status Enable: 使能所有状态位 (用于 poll_int_status 轮询)
        self.write::<u16>(SDHCI_NORM_INT_STS_EN, NORM_INT_ENABLE_MASK);
        self.write::<u16>(SDHCI_ERR_INT_STS_EN, ERR_INT_ENABLE_MASK);
        // Signal Enable: 仅使能 CARD_INT (ISR 只处理 CARD_INT)
        irq::enable_irq_signals();
        Ok(())
    }
}

impl SdioHost for CviSdhci {
    fn init(&mut self) -> Result<(), SdioError> {
        const OCR_IO_FUNC_SHIFT: u32 = 28;
        const OCR_IO_FUNC_MASK: u32 = 0x7 << OCR_IO_FUNC_SHIFT;

        // Step 1: SDHCI 控制器软件复位
        self.controller_reset()?;

        // Step 1.5: 设置数据超时
        self.write::<u8>(SDHCI_TIMEOUT_CONTROL, 0x0E);

        // Step 2: 设置卡检测覆写
        self.setup_card_detect()?;

        // Step 3: 上电 3.3V
        self.power_on()?;

        // Step 3.5: 等待电源稳定
        delay_ms(20);

        // Step 4: 设置初始低速时钟 400KHz
        self.setup_initial_clock()?;

        // Step 4.5: 74+ clocks 稳定时间
        delay_ms(2);

        // Step 5: 使能中断状态位 + CARD_INT 信号
        self.enable_interrupts_irq()?;

        // Step 6: CMD5 探测 SDIO 卡
        let ocr_query = self.send_cmd(5, 0x0000_0000).inspect_err(|_| {
            log::warn!("[SDIO] CMD5 failed: no SDIO card detected");
        })?;
        let num_io_funcs = ((ocr_query & OCR_IO_FUNC_MASK) >> OCR_IO_FUNC_SHIFT) as u8;
        log::debug!(
            "[SDIO] CMD5: {} IO function(s), memory={}",
            num_io_funcs,
            (ocr_query & OCR_MEM_PRESENT) != 0
        );

        // 选择电压并轮询直到就绪
        let voltage = ocr_query & OCR_VOLTAGE_MASK & OCR_3V2_3V4;
        if voltage == 0 {
            log::error!("[SDIO] No common voltage range");
            return Err(SdioError::Unsupported);
        }
        let mut ready = false;
        for _ in 0..CMD5_OCR_RETRY {
            let resp = self.send_cmd(5, voltage)?;
            if resp & R4_READY != 0 {
                ready = true;
                break;
            }
            delay_ms(10);
        }
        if !ready {
            log::error!("[SDIO] Card not ready after CMD5 polling");
            return Err(SdioError::Timeout);
        }
        log::debug!("[SDIO] Card ready (IORDY)");

        delay_ms(10);

        // Step 7: CMD3 获取 RCA
        let resp = self.send_cmd(3, 0)?;
        self.rca = (resp >> 16) as u16;
        log::debug!("[SDIO] RCA = 0x{:04x}", self.rca);

        // Step 8: CMD7 选卡
        self.send_cmd(7, (self.rca as u32) << 16)?;

        delay_ms(10);

        // Step 9: 高速模式
        let bus_speed = self.cmd52_read(0, CCCR_BUS_SPEED_SELECT)?;
        if (bus_speed & 0x01) != 0 {
            self.cmd52_write(0, CCCR_BUS_SPEED_SELECT, bus_speed | 0x02)?;
            let hc1 = self.read::<u8>(SDHCI_HOST_CONTROL);
            self.write::<u8>(SDHCI_HOST_CONTROL, hc1 | HC_HIGH_SPEED);
            self.set_clock(HIGH_SPEED_CLOCK_HZ)?;
            delay_ms(10);
            log::debug!("[SDIO] High-Speed {}Hz enabled", HIGH_SPEED_CLOCK_HZ);
        } else {
            self.set_clock(25_000_000)?;
            delay_ms(10);
        }

        // Step 9.5: VENDOR_MSHC_CTRL — 设置 SD1_SEL (bit16)
        let vendor = self.read::<u32>(VENDOR_MSHC_CTRL);
        self.write::<u32>(VENDOR_MSHC_CTRL, vendor | VENDOR_MSHC_CTRL_SD1_SEL);
        log::info!(
            "[SDIO] VENDOR_MSHC_CTRL: 0x{:08x} -> 0x{:08x}",
            vendor,
            vendor | VENDOR_MSHC_CTRL_SD1_SEL
        );

        // Step 10: 4-bit bus mode
        let bus_if = self.cmd52_read(0, CCCR_BUS_INTERFACE)?;
        self.cmd52_write(0, CCCR_BUS_INTERFACE, (bus_if & 0xFC) | 0x02)?;
        let hc = self.read::<u8>(SDHCI_HOST_CONTROL);
        self.write::<u8>(SDHCI_HOST_CONTROL, hc | HC_BUS_WIDTH_4);

        // Step 11: 使能 Function 1 并设置块大小
        self.enable_func(1)?;
        self.set_block_size(1, SDIO_DEFAULT_BLOCK_SIZE)?;

        // Step 12: 读取 vendor/device ID
        let (vid, did) = self
            .read_manfid_from_cis(1)
            .or_else(|_| self.read_manfid_from_cis(0))?;
        self.vendor_id = vid;
        self.device_id = did;
        log::debug!("[SDIO] card: vendor=0x{:04x}, device=0x{:04x}", vid, did);

        log::debug!("[SDIO] SDHCI init complete");
        Ok(())
    }

    fn mmio_base(&self) -> usize {
        self.base
    }

    fn read_byte(&self, func: u8, addr: u32) -> Result<u8, SdioError> {
        self.cmd52_read(func, addr)
    }

    fn write_byte(&self, func: u8, addr: u32, val: u8) -> Result<(), SdioError> {
        self.cmd52_write(func, addr, val)
    }

    fn write_byte_read(&self, func: u8, addr: u32, val: u8) -> Result<u8, SdioError> {
        self.cmd52(func, addr, CMD52_RW_FLAG | CMD52_RAW_FLAG, val)
    }

    fn read_fifo(&self, func: u8, addr: u32, buf: &mut [u8]) -> Result<(), SdioError> {
        // 512 对齐的走 block 模式;非对齐(如 V3 byte-mode 收帧的 byte_len*4)且 ≤512
        // 的走 byte 模式 CMD53。调用方(aic8800 rx)已保证单次 ≤512。
        let use_blk = buf.len().is_multiple_of(SDIO_DEFAULT_BLOCK_SIZE as usize);
        self.cmd53_read_fixed(func, addr, buf, 512, use_blk)
    }

    fn read_fifo_inc(&self, func: u8, addr: u32, buf: &mut [u8]) -> Result<(), SdioError> {
        let (bs, nb) = self.cmd53_xfer(func, addr, false, true, 512, true, buf.len())?;
        self.pio_read(buf, bs, nb)?;
        self.wait_transfer_complete()
    }

    fn write_fifo(&self, func: u8, addr: u32, buf: &[u8]) -> Result<(), SdioError> {
        self.cmd53_write_fixed(func, addr, buf, 512, true)
    }

    fn write_fifo_inc(&self, func: u8, addr: u32, buf: &[u8]) -> Result<(), SdioError> {
        let (bs, nb) = self.cmd53_xfer(func, addr, true, true, 512, true, buf.len())?;
        self.pio_write(buf, bs, nb)?;
        self.wait_transfer_complete()
    }

    fn set_block_size(&self, func: u8, size: u16) -> Result<(), SdioError> {
        if func > 7 {
            return Err(SdioError::Unsupported);
        }

        if size == 0 || size > 2048 {
            return Err(SdioError::Unsupported);
        }

        let base = 0x100 * (func as u32);
        self.cmd52_write(0, base + 0x10, (size & 0xFF) as u8)?;
        self.cmd52_write(0, base + 0x11, ((size >> 8) & 0xFF) as u8)?;
        let lo = self.cmd52_read(0, base + 0x10)? as u16;
        let hi = self.cmd52_read(0, base + 0x11)? as u16;
        let readback = (hi << 8) | lo;
        if readback != size {
            return Err(SdioError::IoError);
        }

        Ok(())
    }

    fn set_clock(&self, hz: u32) -> Result<(), SdioError> {
        let caps = self.read::<u32>(SDHCI_CAPABILITIES);
        let reported_base_clock =
            ((caps >> CAPS_BASE_FREQ_SHIFT) & CAPS_BASE_FREQ_MASK) * MHZ_TO_HZ;
        let base_clock = CVI_SDIO_SRC_CLOCK_HZ;

        let divisor = if hz >= base_clock {
            0u16
        } else {
            let div = base_clock.div_ceil(DIV_FACTOR * hz);
            div.min(MAX_DIVISOR as u32) as u16
        };

        log::trace!(
            "[SDIO] set_clock target={}Hz source={}Hz reported_source={}Hz divisor={}",
            hz,
            base_clock,
            reported_base_clock,
            divisor
        );

        let mut clk_reg = self.read::<u16>(SDHCI_CLOCK_CONTROL);
        clk_reg &= !(CC_SD_CLK_EN | CC_INT_CLK_EN);
        self.write::<u16>(SDHCI_CLOCK_CONTROL, clk_reg);

        clk_reg &= !(CC_FREQ_SEL_MASK | CC_FREQ_SEL_EXT_MASK);
        let freq_sel = (divisor & DIVISOR_LOW_MASK) << CC_DIV_SHIFT;
        let ext_sel = ((divisor >> 8) & DIVISOR_HIGH_MASK) << CC_EXT_DIV_SHIFT;
        clk_reg |= freq_sel | ext_sel | CC_INT_CLK_EN;
        self.write::<u16>(SDHCI_CLOCK_CONTROL, clk_reg);

        self.wait_clock_stable()?;

        clk_reg = self.read::<u16>(SDHCI_CLOCK_CONTROL);
        self.write::<u16>(SDHCI_CLOCK_CONTROL, clk_reg | CC_SD_CLK_EN);

        Ok(())
    }

    fn enable_func(&self, func: u8) -> Result<(), SdioError> {
        if func == 0 || func > 7 {
            return Err(SdioError::Unsupported);
        }

        let io_en = self.cmd52_read(0, CCCR_IO_ENABLE)?;
        self.cmd52_write(0, CCCR_IO_ENABLE, io_en | (1 << func))?;

        for _ in 0..1000u32 {
            let io_ready = self.cmd52_read(0, CCCR_IO_READY)?;
            if io_ready & (1 << func) != 0 {
                return Ok(());
            }
            delay_ms(1);
        }

        log::error!("SDIO: Function {} not ready after enabling", func);
        Err(SdioError::Timeout)
    }

    fn vendor_device_id(&self) -> (u16, u16) {
        (self.vendor_id, self.device_id)
    }

    fn enable_irq(&self) {
        irq::enable_irq_signals();
    }

    fn disable_irq(&self) {
        irq::disable_irq_signals();
    }

    fn card_irq_ctrl(&self) -> Option<Arc<dyn SdioCardIrq>> {
        Some(Arc::new(CviCardIrqCtrl::new(self.base)))
    }
}
