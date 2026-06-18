//! SDIO 传输层抽象
//!
//! 封装所有 SDIO 操作，屏蔽 Mutex 加锁和 trait 方法分派细节。
//! 上层代码只需调用 `transport.read_byte()` 等方法。

use alloc::sync::Arc;

use sdio_host::{SdioCardIrq, SdioHost};
use spin::Mutex;

use crate::{
    common::{
        ChipVariant, SDIOWIFI_BLOCK_CNT_REG, SDIOWIFI_BYTEMODE_LEN_REG,
        SDIOWIFI_BYTEMODE_LEN_REG_V3, SDIOWIFI_FLOW_CTRL_Q1_REG_V3, SDIOWIFI_FLOW_CTRL_REG,
        SDIOWIFI_FLOWCTRL_MASK, SDIOWIFI_INTR_CONFIG_REG, SDIOWIFI_INTR_ENABLE_REG_V3,
        SDIOWIFI_MISC_INT_STATUS_REG_V3, SDIOWIFI_RD_FIFO_ADDR, SDIOWIFI_RD_FIFO_ADDR_V3,
        SDIOWIFI_SLEEP_REG_V3, SDIOWIFI_V3_SLEEP_READY_BIT, SDIOWIFI_V3_WAKEUP_VALUE,
        SDIOWIFI_WAKEUP_REG_V3, SDIOWIFI_WR_FIFO_ADDR, SDIOWIFI_WR_FIFO_ADDR_V3,
    },
    fdrv::consts::BUFFER_SIZE,
};

/// SDIO 传输层
///
/// 对 `Arc<Mutex<dyn SdioHost>>` 的封装，提供简洁的 SDIO 操作接口。
/// 所有方法内部处理加锁和 trait 方法分派。
pub struct SdioTransport {
    sdio: Arc<Mutex<dyn SdioHost>>,
    card_irq: Option<Arc<dyn SdioCardIrq>>,
    is_v3: bool,
    /// 命令/数据邮箱所在的 SDIO function。DC/DW = 2(真机实测 CFM 在 func2),
    /// 其余 = 1。RX 线程读 block_cnt/RD_FIFO、TX 线程写 WR_FIFO 都用它。
    cmd_func: u8,
}

impl SdioTransport {
    /// 从任意 SdioHost 实现创建 SdioTransport
    pub fn new<H: SdioHost + 'static>(sdio: H, chip: ChipVariant) -> Arc<Self> {
        let card_irq = sdio.card_irq_ctrl();
        let cmd_func = if matches!(chip, ChipVariant::Aic8800DC | ChipVariant::Aic8800DW) {
            2
        } else {
            1
        };
        Arc::new(Self {
            sdio: Arc::new(Mutex::new(sdio)),
            card_irq,
            is_v3: chip.is_v3(),
            cmd_func,
        })
    }

    pub fn is_v3(&self) -> bool {
        self.is_v3
    }

    /// 命令/数据邮箱所在的 SDIO function(DC/DW=2, 其余=1)
    pub fn cmd_func(&self) -> u8 {
        self.cmd_func
    }

    /// 数据/管理帧平面所在的 SDIO function。
    ///
    /// DC/DW 的 SDIO 是双管道:func2 是命令邮箱(命令 TX→CFM RX),
    /// func1 是数据平面(数据/管理帧 TX/RX + 流控,真机实测数据帧 RX 在 func1)。
    /// 命令走 cmd_func()=2,数据/管理帧走本方法=1。非 DC 芯片两者都是 func1。
    pub fn data_func(&self) -> u8 {
        1
    }

    pub fn block_cnt_reg(&self) -> u32 {
        if self.is_v3 {
            SDIOWIFI_MISC_INT_STATUS_REG_V3
        } else {
            SDIOWIFI_BLOCK_CNT_REG
        }
    }

    pub fn flow_ctrl_reg_addr(&self) -> u32 {
        if self.is_v3 {
            SDIOWIFI_FLOW_CTRL_Q1_REG_V3
        } else {
            SDIOWIFI_FLOW_CTRL_REG
        }
    }

    pub fn rd_fifo_addr(&self) -> u32 {
        if self.is_v3 {
            SDIOWIFI_RD_FIFO_ADDR_V3
        } else {
            SDIOWIFI_RD_FIFO_ADDR
        }
    }

    /// byte-mode 长度寄存器:V3 收帧走 byte 模式时,此寄存器值 ×4 即为字节长度。
    pub fn bytemode_len_reg(&self) -> u32 {
        if self.is_v3 {
            SDIOWIFI_BYTEMODE_LEN_REG_V3
        } else {
            SDIOWIFI_BYTEMODE_LEN_REG
        }
    }

    pub fn wr_fifo_addr(&self) -> u32 {
        if self.is_v3 {
            SDIOWIFI_WR_FIFO_ADDR_V3
        } else {
            SDIOWIFI_WR_FIFO_ADDR
        }
    }

    pub fn intr_config_reg_addr(&self) -> u32 {
        if self.is_v3 {
            SDIOWIFI_INTR_ENABLE_REG_V3
        } else {
            SDIOWIFI_INTR_CONFIG_REG
        }
    }

    // ===== 基础 SDIO 操作 =====

    /// CMD52: 单字节读
    pub fn read_byte(&self, func: u8, addr: u32) -> Result<u8, sdio_host::error::SdioError> {
        self.sdio.lock().read_byte(func, addr)
    }

    /// CMD52: 单字节写
    pub fn write_byte(
        &self,
        func: u8,
        addr: u32,
        val: u8,
    ) -> Result<(), sdio_host::error::SdioError> {
        self.sdio.lock().write_byte(func, addr, val)
    }

    /// CMD53: FIFO 读
    pub fn read_fifo(
        &self,
        func: u8,
        addr: u32,
        buf: &mut [u8],
    ) -> Result<(), sdio_host::error::SdioError> {
        self.sdio.lock().read_fifo(func, addr, buf)
    }

    /// CMD53: FIFO 写
    pub fn write_fifo(
        &self,
        func: u8,
        addr: u32,
        buf: &[u8],
    ) -> Result<(), sdio_host::error::SdioError> {
        self.sdio.lock().write_fifo(func, addr, buf)
    }

    // ===== 中断控制 =====

    /// 屏蔽 SDIO 卡中断（CARD_INT）
    ///
    /// 在 SDIO 总线操作（CMD52/CMD53）期间调用，防止 CARD_INT
    /// 电平触发导致 ISR 重入。操作完成后调用 `unmask_card_irq()` 恢复。
    pub(crate) fn mask_card_irq(&self) {
        if let Some(ref ctrl) = self.card_irq {
            ctrl.mask_card_irq();
        }
    }

    /// 恢复 SDIO 卡中断（CARD_INT）
    pub(crate) fn unmask_card_irq(&self) {
        if let Some(ref ctrl) = self.card_irq {
            ctrl.unmask_card_irq();
        }
    }

    /// 使能 SDHCI 中断信号
    pub fn enable_irq(&self) {
        self.sdio.lock().enable_irq();
    }

    /// 禁用 SDHCI 中断信号
    pub fn disable_irq(&self) {
        self.sdio.lock().disable_irq();
    }

    // ===== V3 芯片唤醒 =====

    /// 唤醒 V3 芯片的 SDIO 接口
    ///
    /// V3 固件在空闲后会进入 SDIO 总线级睡眠（与 802.11 PS 无关）。
    /// 睡眠时 FIFO 写入看似成功但固件不会真正发送帧。
    /// Linux 驱动在每次 TX 前调用 `aicwf_sdio_wakeup()` 写入唤醒寄存器。
    ///
    /// 快速路径：芯片已醒 → 直接返回 true
    /// 慢速路径：写 wakeup_reg → 轮询 sleep_reg ready bit（最多 ~50ms）
    pub fn wakeup(&self) -> bool {
        if !self.is_v3 {
            return true;
        }

        if let Ok(val) = self.read_byte(1, SDIOWIFI_SLEEP_REG_V3)
            && val & SDIOWIFI_V3_SLEEP_READY_BIT != 0
        {
            return true;
        }

        if let Err(e) = self.write_byte(1, SDIOWIFI_WAKEUP_REG_V3, SDIOWIFI_V3_WAKEUP_VALUE) {
            log::error!("[sdio] wakeup write failed: {:?}", e);
            return false;
        }

        for _ in 0..200 {
            match self.read_byte(1, SDIOWIFI_SLEEP_REG_V3) {
                Ok(val) if val & SDIOWIFI_V3_SLEEP_READY_BIT != 0 => return true,
                _ => crate::runtime::runtime().yield_now(),
            }
        }

        log::warn!("[sdio] wakeup timeout, chip may still be sleeping");
        false
    }

    // ===== 流控 =====

    /// 读取流控寄存器原始值
    pub fn read_flow_ctrl(&self) -> Result<u8, sdio_host::error::SdioError> {
        self.sdio.lock().read_byte(1, self.flow_ctrl_reg_addr())
    }

    /// 读取流控值（已应用 MASK）
    pub fn read_flow_ctrl_value(&self) -> Result<u8, sdio_host::error::SdioError> {
        self.read_flow_ctrl().map(|fc| fc & SDIOWIFI_FLOWCTRL_MASK)
    }

    /// 检查流控是否可用（fc_val != 0）
    pub fn check_flow_ctrl_available(&self) -> bool {
        matches!(self.read_flow_ctrl_value(), Ok(v) if v != 0)
    }

    /// 检查是否有足够空间发送指定长度的数据
    pub fn check_flow_ctrl_for_size(&self, send_len: usize) -> bool {
        match self.read_flow_ctrl_value() {
            Ok(v) if v != 0 => (v as usize) * BUFFER_SIZE > send_len,
            _ => false,
        }
    }

    /// 等待流控就绪（yield 模式，不占 CPU）
    ///
    /// 当流控不足时让出 CPU，等待 RX 线程处理数据后流控自然恢复，
    /// 而非 busy-wait 旋转浪费 CPU 时间。
    pub fn wait_flow_ctrl(&self, max_retries: u32) -> bool {
        for _ in 0..max_retries {
            if self.check_flow_ctrl_available() {
                return true;
            }
            crate::runtime::runtime().yield_now();
        }
        false
    }

    /// 等待流控就绪（带长度检查，yield 模式）
    pub fn wait_flow_ctrl_for_size(&self, send_len: usize, max_retries: u32) -> bool {
        for _ in 0..max_retries {
            if self.check_flow_ctrl_for_size(send_len) {
                return true;
            }
            crate::runtime::runtime().yield_now();
        }
        false
    }
}
