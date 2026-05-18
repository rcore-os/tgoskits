//! TIU (Tensor Instruction Unit) 寄存器定义和操作

use core::ptr::{read_volatile, write_volatile};

/// BD 引擎命令对齐位
pub const BDC_ENGINE_CMD_ALIGNED_BIT: u32 = 8;

/// BD 控制基地址偏移
pub const BD_CTRL_BASE_ADDR: usize = 0x100;

// ============ BD 控制位 (基于 BD_CTRL_BASE_ADDR) ============

/// TPU 使能位
pub const BD_TPU_EN: u32 = 0;
/// 描述符地址有效位
pub const BD_DES_ADDR_VLD: u32 = 30;
/// TIU 中断全局使能位
pub const BD_INTR_ENABLE: u32 = 31;

/// TIU 寄存器操作
pub struct TiuRegs {
    base: *mut u8,
}

// SAFETY: TIU 寄存器访问是通过内存映射进行的，可以安全地在线程间共享
// 寄存器访问本身是原子的，多线程访问需要在更高层进行同步
unsafe impl Sync for TiuRegs {}
unsafe impl Send for TiuRegs {}

impl TiuRegs {
    /// 创建 TIU 寄存器操作实例
    ///
    /// # Safety
    /// 调用者必须确保 base 指向有效的 TIU 寄存器映射地址
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

    /// 读取 BD 控制寄存器
    #[inline]
    pub fn read_bd_ctrl(&self, offset: usize) -> u32 {
        self.read(BD_CTRL_BASE_ADDR + offset)
    }

    /// 写入 BD 控制寄存器
    #[inline]
    pub fn write_bd_ctrl(&self, offset: usize, value: u32) {
        self.write(BD_CTRL_BASE_ADDR + offset, value)
    }

    /// 获取当前 BD 命令 ID
    pub fn get_current_bd_id(&self) -> u32 {
        (self.read_bd_ctrl(0) >> 6) & 0xFFFF
    }

    /// 检查 BD 中断是否触发
    pub fn is_bd_interrupt(&self) -> bool {
        (self.read_bd_ctrl(0) & (1 << 1)) != 0
    }

    /// 清除 BD 中断
    pub fn clear_bd_interrupt(&self) {
        let reg_val = self.read_bd_ctrl(0);
        self.write_bd_ctrl(0, reg_val | (1 << 1));
    }

    /// 重置 TIU ID
    pub fn reset_id(&self) {
        // 重置 TIU ID
        let reg_val = self.read_bd_ctrl(0xC);
        self.write_bd_ctrl(0xC, reg_val | 0x1);
        self.write_bd_ctrl(0xC, reg_val & !0x1);

        // 禁用 TPU 和描述符模式
        let reg_val = self.read_bd_ctrl(0);
        self.write_bd_ctrl(0, reg_val & !((1 << BD_TPU_EN) | (1 << BD_DES_ADDR_VLD)));

        // 重置中断状态
        let reg_val = self.read_bd_ctrl(0);
        self.write_bd_ctrl(0, reg_val | (1 << 1));
    }

    /// 设置并启动 TIU 描述符执行
    pub fn fire_descriptor(&self, desc_offset: u64, _num_bd: u32) {
        let desc_addr = desc_offset << BDC_ENGINE_CMD_ALIGNED_BIT;

        // 设置描述符地址
        self.write_bd_ctrl(0x4, (desc_addr & 0xFFFFFFFF) as u32);
        let reg_val = self.read_bd_ctrl(0x8);
        self.write_bd_ctrl(
            0x8,
            (reg_val & 0xFFFFFF00) | ((desc_addr >> 32) as u32 & 0xFF),
        );

        // 禁用 TIU pre_exe
        let reg_val = self.read_bd_ctrl(0xC);
        self.write_bd_ctrl(0xC, reg_val | (1 << 11));

        // 设置 1 array, lane=8
        let reg_val = self.read_bd_ctrl(0);
        let reg_val = reg_val & !0x3FC00000;
        self.write_bd_ctrl(0, reg_val | (3 << 22));

        // 启动 TIU
        let reg_val = self.read_bd_ctrl(0);
        self.write_bd_ctrl(
            0,
            reg_val | (1 << BD_DES_ADDR_VLD) | (1 << BD_INTR_ENABLE) | (1 << BD_TPU_EN),
        );
    }
}
