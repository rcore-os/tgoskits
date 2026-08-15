//! CVI SoC (CV1800/SG2002) SDHCI 控制器驱动
//!
//! 职责:
//!   - SDHCI 标准寄存器操作 (CMD52/CMD53/PIO)
//!   - SDIO 卡枚举 (CMD5/CMD3/CMD7)
//!   - 中断处理 (ISR 处理 CARD_INT + XFER_COMPLETE)
//!   - 时钟/电源/总线宽度配置
//!
//! 设计:
//!   - ISR: 处理 CARD_INT (通知 WiFi 驱动) + XFER_COMPLETE (唤醒阻塞任务)
//!   - PIO: wait_* 方法 Phase 1 轮询 INT_STATUS, Phase 2 中断驱动等待
//!   - 丢唤醒防护: Phase 2 阻塞走 `SdhciDelay::block_timeout_until`，
//!     条件检查与任务入队在同一关中断临界区内衔接，配合 ISR 不消费的
//!     XFER_COMPLETE sticky 位——ISR 无论在锁内检查前触发（notify 落空、
//!     sticky 位被观察到）、检查后入队前（不可能，临界区关中断）还是
//!     入队后（notify 命中队列），事件都不会错过。
//!   - 单核串行化: ISR 与任务在单 hart 上执行。SIG_EN RMW 仍可被 ISR
//!     抢占，但事件由 sticky 位锁存（详见 irq.rs 模块文档）。

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

/// Phase 1 快速自旋迭代次数（~50µs on C906 @1GHz）
const PHASE1_SPIN_ITERS: u32 = 1000;
/// Phase 2 单次等待时长 (ms)
const PHASE2_STEP_MS: u64 = 10;
/// Phase 2 最大迭代次数（总预算 = 20 × 10ms = 200ms）
const PHASE2_MAX_ITERS: u32 = 20;
/// Phase 2 中段警告阈值（迭代次数）
const PHASE2_WARN_AT: u32 = 10;

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

    /// 仅 W1C 清除 INT_STATUS_NORM 中指定的位。
    /// 绝不清除 CARD_INT——该位由 ISR/mask 协议独占管理。
    fn clear_int_status_norm(&self, bits: u16) {
        self.write::<u16>(SDHCI_INT_STATUS_NORM, bits);
    }

    /// 轮询 INT_STATUS 一次：检查错误或目标位，命中时消费。
    /// 返回：
    /// - `Some(Ok(()))` 目标位置位（W1C 清除）
    /// - `Some(Err(...))` 检测到错误中断（W1C 清除 + DAT 复位）
    /// - `None` 两个条件均未满足（继续轮询/等待）
    fn poll_status_once(&self, bit: u16) -> Option<Result<(), SdioError>> {
        let norm = self.read::<u16>(SDHCI_INT_STATUS_NORM);
        if norm & NORM_INT_ERROR != 0 {
            let err = self.read::<u16>(SDHCI_INT_STATUS_ERR);
            self.write::<u16>(SDHCI_INT_STATUS_ERR, err);
            // 选择性清除：错误位 + 等待位 + XFER_COMPLETE。
            // XFER_COMPLETE 可能与错误同时被置位（如数据阶段完成后 DAT 错误）。
            // 在此消费可防止 stale bit 泄漏至下一传输的 wait_transfer_complete
            // 导致虚假过早成功。CARD_INT 有意保留（ISR/mask 协议）。
            self.clear_int_status_norm(NORM_INT_ERROR | bit | NORM_INT_XFER_COMPLETE);
            self.reset_dat_line();
            return Some(Err(Self::classify_error(err)));
        }
        if norm & bit != 0 {
            self.clear_int_status_norm(bit);
            return Some(Ok(()));
        }
        None
    }

    /// 轮询 INT_STATUS_NORM，等待指定 bit 置位后选择性 W1C 清除。
    ///
    /// Phase 1: 快速自旋 (PHASE1_SPIN_ITERS 次, ~50µs)
    /// Phase 2: XFER_COMPLETE 走硬件中断驱动等待；其他位使用 10ms 睡眠轮询
    ///
    /// 同时检测 Error 中断：如果 ERROR bit (bit 15) 置位，
    /// 读取 ERR_STATUS，选择性 W1C 清除错误位 + 当前等待位（保留 CARD_INT），
    /// 然后复位 DAT 线并返回错误。
    fn poll_int_status(&self, bit: u16) -> Result<(), SdioError> {
        // 在进入 Phase 1 自旋循环前排空存储缓冲区。
        // 若无此栅栏，待处理的 MMIO 写（如 pio_write 的 128 次 SDHCI_BUFFER
        // 写入）可能仍排在 CPU 存储缓冲区中，而此时后续的
        // mmio_read(INT_STATUS_NORM) 循环已开始。读取与排空写入在 SDHCI
        // 总线上竞争，延迟硬件实际接收数据并置位 BUF_WR_READY/CMD_COMPLETE。
        // Phase 1 的 1000 次迭代窗口（约 50µs）可能在状态位可见前过期，
        // 导致落入 10ms Phase 2 延迟。此处单一栅栏保证轮询开始前存储缓冲区
        // 已空——与旧 yield_now 忙等待方案中任务切换隐含栅栏（mret）效果相同。
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        // Phase 1: 快速自旋
        for _ in 0..PHASE1_SPIN_ITERS {
            if let Some(result) = self.poll_status_once(bit) {
                return result;
            }
            core::hint::spin_loop();
        }
        // Phase 2: XFER_COMPLETE 走硬件中断驱动等待；其他位走纯超时睡眠
        // （不经过 WaitQueue——ISR 不会为这些位发 notify，经过 WQ 是无效开销）
        let use_irq = bit == NORM_INT_XFER_COMPLETE;
        if !use_irq {
            log::trace!("[SDHCI] poll_int Phase-2 fallback: bit=0x{:04x}", bit);
        }
        let mut timeout_count: u32 = 0;
        for i in 0..PHASE2_MAX_ITERS {
            // 阻塞前先检查状态寄存器（快路径）。此 pre-check 不能单独关闭
            // 丢唤醒窗口——真正的防护是下方的 block_timeout_until 协议：
            // 条件检查与任务入队在同一关中断临界区内衔接，unmask 与入队
            // 之间的 ISR 事件由 XFER_COMPLETE sticky 位保留并被锁内条件
            // 检查观察到（见 SdhciDelay 契约）。
            if let Some(result) = self.poll_status_once(bit) {
                return result;
            }

            if i == PHASE2_WARN_AT {
                let pres = self.read::<u32>(SDHCI_PRESENT_STATE);
                let sts = self.read::<u16>(SDHCI_INT_STATUS_NORM);
                log::warn!(
                    "[SDHCI] poll_int mid-timeout: bit=0x{:04x} PRES=0x{:08x} INT_STS=0x{:04x} \
                     timeouts={}",
                    bit,
                    pres,
                    sts,
                    timeout_count
                );
            }

            if use_irq {
                irq::unmask_xfer_complete_signal();
                // 条件等待：ISR 若在 unmask 后、锁内检查前触发，其 notify
                // 落空（队列为空），但它 latch 的 sticky 位会被胶水层锁内
                // 条件检查立即观察到，任务无需等满超时。错误位也纳入条件：
                // 错误不产生中断（SIG_EN 不含 error 位），锁内检查前已锁存
                // 的错误可即时返回；睡期中段到达的错误仍由 10ms 超时后的
                // post-wake 重检查出，与旧实现时延一致。
                let timed_out =
                    crate::runtime::delay().block_timeout_until(PHASE2_STEP_MS, &|| {
                        let norm = self.read::<u16>(SDHCI_INT_STATUS_NORM);
                        norm & (bit | NORM_INT_ERROR) != 0
                    });
                // 返回值无需驱动分支——post-wake 重检无条件执行，覆盖两种
                // 返回路径。仅在超时（10ms 退化路径）时留观测信号。
                if timed_out {
                    log::trace!(
                        "[SDHCI] poll_int Phase-2 IRQ wait timed out: bit=0x{:04x}",
                        bit
                    );
                }
            } else {
                // 非 XFER 位：直接 sleep，不经过 WaitQueue——
                // ISR 只对 XFER_COMPLETE 发 notify，经过 WQ 是无效开销。
                crate::runtime::delay().delay_ms(PHASE2_STEP_MS);
            }
            timeout_count += 1;

            // 被唤醒后检查状态寄存器
            if let Some(result) = self.poll_status_once(bit) {
                return result;
            }
        }
        let pres = self.read::<u32>(SDHCI_PRESENT_STATE);
        let sts = self.read::<u16>(SDHCI_INT_STATUS_NORM);
        log::error!(
            "[SDHCI] poll_int_status timeout: bit=0x{:04x} PRES=0x{:08x} INT_STS=0x{:04x} \
             timeouts={}",
            bit,
            pres,
            sts,
            timeout_count
        );
        // 超时后总线可能仍处于 DAT-busy(PRES bit1 DATA_INHIBIT 置位),若不复位
        // DAT 线状态机,后续任何数据命令的 wait_data_idle 都会一直超时,整条 SDIO
        // 总线被焊死(连 WiFi 模式切回 AP 也起不来)。这里对齐错误中断分支:选择性
        // 清除错误位 + 当前等待位 + XFER_COMPLETE（防止 stale bit 泄漏到下一传输），
        // 保留 CARD_INT，然后复位 DAT 线。
        self.clear_int_status_norm(NORM_INT_ERROR | bit | NORM_INT_XFER_COMPLETE);
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

    /// 在启动命令前清除残留的 INT_STATUS 位。
    ///
    /// 使用选择性 W1C：保留 XFER_COMPLETE（可能被阻塞在
    /// `poll_int_status` Phase 2 的任务消费）。ISR 同样不清除
    /// XFER_COMPLETE——仅等待任务的 recheck 在 `poll_int_status`
    /// 中可消费它。
    fn clear_stale_status(&self) {
        let norm = self.read::<u16>(SDHCI_INT_STATUS_NORM);
        // Mask 掉 XFER_COMPLETE——它属于可能阻塞的 PIO waiter，
        // 不得被命令路径破坏。
        let clearable = norm & !NORM_INT_XFER_COMPLETE;
        if clearable != 0 {
            if clearable & NORM_INT_ERROR != 0 {
                let err = self.read::<u16>(SDHCI_INT_STATUS_ERR);
                if err != 0 {
                    self.write::<u16>(SDHCI_INT_STATUS_ERR, err);
                }
            }
            self.write::<u16>(SDHCI_INT_STATUS_NORM, clearable);
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

    /// 使能中断状态位 + CARD_INT 信号。
    /// XFER_COMPLETE 信号由 poll_int_status 阻塞前通过 unmask_xfer_complete_signal 动态启用。
    fn enable_interrupts_irq(&self) -> Result<(), SdioError> {
        irq::irq_state_init(self.base);
        // 状态使能：使能所有状态位（用于 poll_int_status 轮询）
        self.write::<u16>(SDHCI_NORM_INT_STS_EN, NORM_INT_ENABLE_MASK);
        self.write::<u16>(SDHCI_ERR_INT_STS_EN, ERR_INT_ENABLE_MASK);
        // 信号使能：仅使能 CARD_INT；XFER_COMPLETE 由 poll_int_status 动态 un-mask
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

#[cfg(test)]
mod tests {
    //! 丢唤醒协议回归验证。
    //!
    //! 全局状态注意：`set_delay` 与 `irq_state_init` 安装的是进程级全局
    //! provider/基地址，不同测试会相互覆盖，因此新增场景必须并入下面的
    //! 单一测试函数内顺序执行。
    //!
    //! 建模局限（记录在案）：
    //! - W1C 未建模：对 INT_STATUS 的写是直接覆写而非"写 1 清位"。
    //!   当前断言（返回结果、零睡眠、SIG_EN 状态）不受影响；
    //!   场景切换时由测试显式清零寄存器区。

    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};

    use super::*;
    use crate::runtime::{SdhciDelay, set_delay};

    /// 泄漏一块寄存器缓冲区，仅以裸指针访问（避免与 `&mut` 别名）。
    /// 32 × u64 = 256 字节，8 字节对齐保证 u16/u32 访问满足对齐要求。
    fn fake_regs() -> usize {
        Box::leak(Box::new([0u64; 0x20])) as *mut [u64; 0x20] as usize
    }

    /// 重放模式：窗口内 XFER ISR（mask + notify 落空）。
    const MODE_XFER_ISR: u8 = 0;
    /// 重放模式：错误位锁存（错误不产生 ISR，SIG_EN 不变）。
    const MODE_ERROR_LATCH: u8 = 1;

    /// 确定性重放 Phase 2 窗口内的硬件/ISR 事件的 fake 延时提供者。
    ///
    /// `block_timeout_until` 被调用时，重放事件已经发生（对应真实时序中
    /// "unmask 之后、锁内条件检查之前"的窗口），随后按胶水层语义
    /// （关中断临界区内）检查条件：
    ///
    /// - MODE_XFER_ISR：硬件置位 XFER_COMPLETE sticky 位，ISR mask 信号
    ///   并 notify——此时任务尚未入队，notify 落空丢失。
    /// - MODE_ERROR_LATCH：硬件锁存错误位（NORM_ERROR + DAT_TIMEOUT），
    ///   错误不产生中断，无 ISR、无 mask。
    struct FakeIrqDelay {
        base: AtomicUsize,
        slept_ms: AtomicU64,
        mode: AtomicU8,
    }

    impl FakeIrqDelay {
        fn replay_event_in_window(&self) {
            let base = self.base.load(Ordering::Acquire);
            let sig_en_addr = base + SDHCI_NORM_INT_SIG_EN as usize;
            // 前置断言：unmask 必须先于阻塞发生（SIG_EN 已含 XFER 位），
            // 拦截"block 先于 unmask"的顺序回归。
            let sig_en = mmio_read::<u16>(sig_en_addr);
            assert!(
                sig_en & NORM_INT_XFER_COMPLETE != 0,
                "unmask_xfer_complete_signal 必须先于 block_timeout_until 执行"
            );

            let norm_addr = base + SDHCI_INT_STATUS_NORM as usize;
            let norm = mmio_read::<u16>(norm_addr);
            match self.mode.load(Ordering::Acquire) {
                MODE_ERROR_LATCH => {
                    // 硬件锁存错误位；错误不产生中断，SIG_EN 不变。
                    mmio_write::<u16>(norm_addr, norm | NORM_INT_ERROR);
                    let err_addr = base + SDHCI_INT_STATUS_ERR as usize;
                    let err = mmio_read::<u16>(err_addr);
                    mmio_write::<u16>(err_addr, err | ERR_INT_DAT_TIMEOUT);
                }
                _ => {
                    // 硬件在 unmask 后立即置位 XFER_COMPLETE sticky 位。
                    mmio_write::<u16>(norm_addr, norm | NORM_INT_XFER_COMPLETE);
                    // ISR mask XFER 信号（RMW 清除 XFER 位）。
                    mmio_write::<u16>(sig_en_addr, sig_en & !NORM_INT_XFER_COMPLETE);
                    // notify：队列为空，通知丢失（无动作）。
                }
            }
        }
    }

    impl SdhciDelay for FakeIrqDelay {
        fn delay_ms(&self, ms: u64) {
            self.slept_ms.fetch_add(ms, Ordering::Relaxed);
        }

        fn block_timeout_until(&self, timeout_ms: u64, condition: &dyn Fn() -> bool) -> bool {
            self.replay_event_in_window();
            if condition() {
                return false;
            }
            // 条件未观察到事件 → 协议失效路径：按真实语义睡满超时。
            self.delay_ms(timeout_ms);
            true
        }
    }

    /// Phase 2 IRQ 等待的两个确定性回归场景：
    ///
    /// - 场景 A：ISR 在 unmask 之后、锁内检查之前触发（notify 落空），
    ///   XFER_COMPLETE 等待必须即时返回，不得退化为睡满 10ms 超时兜底。
    /// - 场景 B：错误位在锁内检查前锁存时，条件立即返回错误路径，
    ///   且错误不产生 ISR。
    #[test]
    fn phase2_wait_lost_wakeup_and_error_latch_regressions() {
        static FAKE_DELAY: FakeIrqDelay = FakeIrqDelay {
            base: AtomicUsize::new(0),
            slept_ms: AtomicU64::new(0),
            mode: AtomicU8::new(MODE_XFER_ISR),
        };

        let base = fake_regs();
        FAKE_DELAY.base.store(base, Ordering::Release);
        FAKE_DELAY.slept_ms.store(0, Ordering::Relaxed);
        set_delay(&FAKE_DELAY);
        irq::irq_state_init(base);

        let sdhci = CviSdhci::new(base);

        // ── 场景 A：ISR 先于入队（notify 落空）──
        FAKE_DELAY.mode.store(MODE_XFER_ISR, Ordering::Release);
        let result = sdhci.poll_int_status(NORM_INT_XFER_COMPLETE);

        assert_eq!(result, Ok(()));
        // 关键断言：等待未落入 10ms 超时兜底——ISR 丢失的 notify 由
        // 锁内条件检查对 sticky 位的观察补偿，事件即时消费。
        assert_eq!(FAKE_DELAY.slept_ms.load(Ordering::Relaxed), 0);
        // 锁定等待后 SIG_EN 的 XFER 位保持 mask 稳态——配合 replay 入口
        // 断言（unmask 已置位）证明 unmask→ISR mask 序列完整执行。
        let sig_en = mmio_read::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize);
        assert_eq!(sig_en & NORM_INT_XFER_COMPLETE, 0);

        // ── 场景 B：错误位锁存（错误路径即时返回）──
        // 先锁定 STS_EN 门控：enable_interrupts_irq 后 STS_EN 必须含
        // NORM_INT_ERROR，否则错误场景在真实硬件上无意义（错误位不锁存）。
        sdhci.enable_interrupts_irq().unwrap();
        let sts_en = mmio_read::<u16>(base + SDHCI_NORM_INT_STS_EN as usize);
        assert_ne!(
            sts_en & NORM_INT_ERROR,
            0,
            "NORM_INT_ERROR 必须已加入 STS_EN 掩码，否则错误检测路径恒假"
        );
        // 清掉场景 A 的覆写残留（fake 不建模 W1C，见模块文档）。
        mmio_write::<u16>(base + SDHCI_INT_STATUS_NORM as usize, 0);
        FAKE_DELAY.mode.store(MODE_ERROR_LATCH, Ordering::Release);
        FAKE_DELAY.slept_ms.store(0, Ordering::Relaxed);

        let result = sdhci.poll_int_status(NORM_INT_XFER_COMPLETE);

        assert_eq!(result, Err(SdioError::Timeout));
        assert_eq!(FAKE_DELAY.slept_ms.load(Ordering::Relaxed), 0);
        // 错误不产生 ISR：SIG_EN 的 XFER 位未被 mask。
        let sig_en = mmio_read::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize);
        assert_ne!(sig_en & NORM_INT_XFER_COMPLETE, 0);
    }
}
