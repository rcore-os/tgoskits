//! TDMA 寄存器定义和操作

use core::ptr::{read_volatile, write_volatile};

/// TDMA 描述符寄存器字节数
pub const TDMA_DESC_REG_BYTES: usize = 0x40;

/// TDMA 引擎描述符数量
pub const TDMA_ENGINE_DESCRIPTOR_NUM: usize = TDMA_DESC_REG_BYTES >> 2;

/// TDMA 基地址寄存器数量
pub const TDMA_NUM_BASE_REGS: usize = 0x8;

// ============ TDMA 寄存器偏移 ============

/// TDMA 控制寄存器
pub const TDMA_CTRL: usize = 0x0;
/// TDMA 描述符基地址
pub const TDMA_DES_BASE: usize = 0x4;
/// TDMA 中断掩码
pub const TDMA_INT_MASK: usize = 0x8;
/// TDMA 同步状态
pub const TDMA_SYNC_STATUS: usize = 0xC;
/// TDMA 命令接收寄存器 0-15
pub const TDMA_CMD_ACCP0: usize = 0x10;
pub const TDMA_CMD_ACCP1: usize = 0x14;
pub const TDMA_CMD_ACCP2: usize = 0x18;
pub const TDMA_CMD_ACCP3: usize = 0x1C;
pub const TDMA_CMD_ACCP4: usize = 0x20;
pub const TDMA_CMD_ACCP5: usize = 0x24;
pub const TDMA_CMD_ACCP6: usize = 0x28;
pub const TDMA_CMD_ACCP7: usize = 0x2C;
pub const TDMA_CMD_ACCP8: usize = 0x30;
pub const TDMA_CMD_ACCP9: usize = 0x34;
pub const TDMA_CMD_ACCP10: usize = 0x38;
pub const TDMA_CMD_ACCP11: usize = 0x3C;
pub const TDMA_CMD_ACCP12: usize = 0x40;
pub const TDMA_CMD_ACCP13: usize = 0x44;
pub const TDMA_CMD_ACCP14: usize = 0x48;
pub const TDMA_CMD_ACCP15: usize = 0x4C;

/// Array base 寄存器 (低32位)
pub const TDMA_ARRAYBASE0_L: usize = 0x70;
pub const TDMA_ARRAYBASE1_L: usize = 0x74;
pub const TDMA_ARRAYBASE2_L: usize = 0x78;
pub const TDMA_ARRAYBASE3_L: usize = 0x7C;
pub const TDMA_ARRAYBASE4_L: usize = 0x80;
pub const TDMA_ARRAYBASE5_L: usize = 0x84;
pub const TDMA_ARRAYBASE6_L: usize = 0x88;
pub const TDMA_ARRAYBASE7_L: usize = 0x8C;

/// Array base 寄存器 (高32位)
pub const TDMA_ARRAYBASE0_H: usize = 0x90;
pub const TDMA_ARRAYBASE1_H: usize = 0x94;

/// TDMA 调试模式
pub const TDMA_DEBUG_MODE: usize = 0xA0;
/// TDMA DCM 禁用
pub const TDMA_DCM_DISABLE: usize = 0xA4;
/// TDMA 状态
pub const TDMA_STATUS: usize = 0xEC;

// ============ TPU PMU 寄存器偏移 ============

/// PMU 控制寄存器
pub const TPUPMU_CTRL: usize = 0x200;
/// PMU buffer 基地址
pub const TPUPMU_BUFBASE: usize = 0x20C;
/// PMU buffer 大小
pub const TPUPMU_BUFSIZE: usize = 0x210;

// ============ TDMA 控制位 ============

/// TDMA 使能位
pub const TDMA_CTRL_ENABLE_BIT: u32 = 0;
/// TDMA 模式选择位
pub const TDMA_CTRL_MODESEL_BIT: u32 = 1;
/// TDMA 重置同步 ID 位
pub const TDMA_CTRL_RESET_SYNCID_BIT: u32 = 2;
/// 强制 1 array 模式
pub const TDMA_CTRL_FORCE_1ARRAY: u32 = 5;
/// 强制 2 array 模式
pub const TDMA_CTRL_FORCE_2ARRAY: u32 = 6;
/// Burst 长度位
pub const TDMA_CTRL_BURSTLEN_BIT: u32 = 8;
/// 64字节对齐使能
pub const TDMA_CTRL_64BYTE_ALIGN_EN: u32 = 10;
/// Intra 命令关闭
pub const TDMA_CTRL_INTRA_CMD_OFF: u32 = 13;
/// 描述符数量位
pub const TDMA_CTRL_DESNUM_BIT: u32 = 16;

// ============ TDMA 中断相关 ============

/// TDMA 中断掩码初始值 (忽略 nchw/stride=0 错误)
pub const TDMA_MASK_INIT: u32 = 0x20;
/// TDMA 描述符结束中断
pub const TDMA_INT_EOD: u32 = 0x1;
/// TDMA PMU 结束中断
pub const TDMA_INT_EOPMU: u32 = 0x8000;
/// TDMA 全部空闲状态
pub const TDMA_ALL_IDLE: u32 = 0x1F;

/// TDMA 寄存器操作
pub struct TdmaRegs {
    base: *mut u8,
}

// SAFETY: TDMA 寄存器访问是通过内存映射进行的，可以安全地在线程间共享
// 寄存器访问本身是原子的，多线程访问需要在更高层进行同步
unsafe impl Sync for TdmaRegs {}
unsafe impl Send for TdmaRegs {}

impl TdmaRegs {
    /// 创建 TDMA 寄存器操作实例
    ///
    /// # Safety
    /// 调用者必须确保 base 指向有效的 TDMA 寄存器映射地址
    pub const unsafe fn new(base: *mut u8) -> Self {
        Self { base }
    }

    /// 读取寄存器
    #[inline]
    pub fn read(&self, offset: usize) -> u32 {
        unsafe { read_volatile((self.base as usize + offset) as *const u32) }
    }

    /// 写入寄存器
    #[inline]
    pub fn write(&self, offset: usize, value: u32) {
        unsafe { write_volatile((self.base as usize + offset) as *mut u32, value) }
    }

    /// 获取基地址
    pub fn base(&self) -> *mut u8 {
        self.base
    }

    /// 获取中断状态
    pub fn get_int_status(&self) -> u32 {
        let reg_value = self.read(TDMA_INT_MASK);
        (reg_value >> 16) & !TDMA_MASK_INIT
    }

    /// 清除中断
    pub fn clear_interrupt(&self) {
        self.write(TDMA_INT_MASK, 0xFFFF0000);
    }

    /// 获取同步状态中的 TDMA ID
    pub fn get_sync_tdma_id(&self) -> u32 {
        self.read(TDMA_SYNC_STATUS) >> 16
    }

    /// 设置 array base 寄存器
    pub fn set_array_bases(&self, header: &super::types::DmaHeader) {
        self.write(TDMA_ARRAYBASE0_L, header.arraybase_0_l);
        self.write(TDMA_ARRAYBASE1_L, header.arraybase_1_l);
        self.write(TDMA_ARRAYBASE2_L, header.arraybase_2_l);
        self.write(TDMA_ARRAYBASE3_L, header.arraybase_3_l);
        self.write(TDMA_ARRAYBASE4_L, header.arraybase_4_l);
        self.write(TDMA_ARRAYBASE5_L, header.arraybase_5_l);
        self.write(TDMA_ARRAYBASE6_L, header.arraybase_6_l);
        self.write(TDMA_ARRAYBASE7_L, header.arraybase_7_l);
        // 假设高位始终为 0
        self.write(TDMA_ARRAYBASE0_H, 0);
        self.write(TDMA_ARRAYBASE1_H, 0);
    }

    /// 重置同步 ID
    pub fn reset_sync_id(&self) {
        self.write(TDMA_CTRL, 1 << TDMA_CTRL_RESET_SYNCID_BIT);
        self.write(TDMA_CTRL, 0);
        // 重置中断状态
        self.write(TDMA_INT_MASK, 0xFFFF0000);
    }

    /// 启动 TDMA 描述符执行
    pub fn fire_descriptor(&self, desc_offset: u64, num_tdma: u32) {
        // 设置描述符地址
        self.write(TDMA_DES_BASE, desc_offset as u32);
        // 确保调试模式禁用
        self.write(TDMA_DEBUG_MODE, 0);
        // 启用 DCM
        self.write(TDMA_DCM_DISABLE, 0);
        // 初始化中断掩码
        self.write(TDMA_INT_MASK, TDMA_MASK_INIT);

        // 启动 TDMA
        let ctrl = (1 << TDMA_CTRL_ENABLE_BIT)
            | (1 << TDMA_CTRL_MODESEL_BIT)
            | (num_tdma << TDMA_CTRL_DESNUM_BIT)
            | (0x3 << TDMA_CTRL_BURSTLEN_BIT)
            | (1 << TDMA_CTRL_FORCE_1ARRAY)
            | (1 << TDMA_CTRL_INTRA_CMD_OFF)
            | (1 << TDMA_CTRL_64BYTE_ALIGN_EN);
        self.write(TDMA_CTRL, ctrl);
    }
}
