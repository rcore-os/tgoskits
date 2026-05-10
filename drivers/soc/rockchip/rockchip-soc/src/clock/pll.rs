/// PLL 类型枚举
///
/// 参考 rockchip_pll_type 定义
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RockchipPllType {
    /// RK3036/3366/3368 类型 PLL
    Rk3036,
    /// RK3066 类型 PLL
    Rk3066,
    /// RK3399 类型 PLL
    Rk3399,
    /// RV1108 类型 PLL
    Rv1108,
    /// RK3588 类型 PLL
    #[default]
    Rk3588,
}

/// PLL 速率表项
///
/// 用于描述 PLL 在不同频率下的配置参数
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PllRateTable {
    /// 输出频率 (Hz)
    pub rate: u64,
    /// PLL 特定参数 (根据芯片类型)
    pub params: PllRateParams,
}

/// PLL 速率参数 (根据芯片类型)
#[derive(Debug, Clone, Copy)]
pub enum PllRateParams {
    Normal {
        /// 参考分频系数 (Reference Divider)
        nr: u32,

        /// 反馈分频系数 (Feedback Divider)
        f: u32,

        /// 输出分频系数 (Output Divider)
        no: u32,

        /// 带宽分频系数 (Bandwidth Divider)
        nb: u32,
    },

    /// RK3036/RK3399 类型参数
    Rk3036 {
        /// 反馈分频系数
        fbdiv: u32,
        /// 后分频器 1
        postdiv1: u32,
        /// 参考分频系数
        refdiv: u32,
        /// 后分频器 2
        postdiv2: u32,
        /// 小数分频使能 (0=启用, 1=禁用)
        dsmpd: u32,
        /// 小数分频系数
        frac: u32,
    },

    /// RK3588 类型参数
    Rk3588 {
        /// M 分频系数 (Main Divider)
        m: u32,
        /// P 分频系数 (Pre-divider)
        p: u32,
        /// S 分频系数 (Post-divider)
        s: u32,
        /// K 小数分频系数
        k: u32,
    },
}

/// Rockchip PLL 时钟结构
#[derive(Debug, Default)]
#[repr(C)]
pub struct PllClock {
    /// 时钟 ID
    pub id: u32,

    /// PLL 控制寄存器偏移量
    pub con_offset: u32,

    /// 模式寄存器偏移量
    pub mode_offset: u32,

    /// 模式位偏移
    pub mode_shift: u32,

    /// 锁定位偏移
    pub lock_shift: u32,

    /// PLL 类型
    pub pll_type: RockchipPllType,

    /// PLL 标志位 (参见 pll_flags 模块)
    pub pll_flags: u32,

    /// PLL 速率表指针
    pub rate_table: &'static [PllRateTable],

    /// 模式掩码
    pub mode_mask: u32,
}

impl PllClock {
    /// 检查 PLL 是否已锁定
    ///
    /// # 参数
    ///
    /// * `base` - CRU 基地址
    ///
    /// # 返回
    ///
    /// 如果 PLL 已锁定返回 `true`,否则返回 `false`
    #[must_use]
    pub fn is_locked(&self, base: usize) -> bool {
        let reg_addr = base + self.con_offset as usize;
        unsafe {
            let reg = reg_addr as *const u32;
            let val = core::ptr::read_volatile(reg);
            (val & (1 << self.lock_shift)) != 0
        }
    }

    /// 获取 PLL 当前模式
    ///
    /// # 参数
    ///
    /// * `base` - CRU 基地址
    ///
    /// # 返回
    ///
    /// 当前模式值
    #[must_use]
    pub fn get_mode(&self, base: usize) -> u32 {
        let reg_addr = base + self.mode_offset as usize;
        unsafe {
            let reg = reg_addr as *const u32;
            let val = core::ptr::read_volatile(reg);
            (val & self.mode_mask) >> self.mode_shift
        }
    }

    /// 设置 PLL 模式
    ///
    /// # 参数
    ///
    /// * `base` - CRU 基地址
    /// * `mode` - 要设置的模式值
    pub fn set_mode(&self, base: usize, mode: u32) {
        let reg_addr = base + self.mode_offset as usize;
        unsafe {
            let reg = reg_addr as *mut u32;
            let current = core::ptr::read_volatile(reg);
            let new_val =
                (current & !self.mode_mask) | ((mode << self.mode_shift) & self.mode_mask);
            core::ptr::write_volatile(reg, new_val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pll_type_values() {
        // 验证枚举值的整型对应关系
        assert_eq!(RockchipPllType::Rk3036 as u32, 0);
        assert_eq!(RockchipPllType::Rk3066 as u32, 1);
        assert_eq!(RockchipPllType::Rk3399 as u32, 2);
        assert_eq!(RockchipPllType::Rv1108 as u32, 3);
        assert_eq!(RockchipPllType::Rk3588 as u32, 4);
    }
}
