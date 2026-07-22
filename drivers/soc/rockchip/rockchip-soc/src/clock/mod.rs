use crate::{Mmio, RstId, SocType};

mod error;
pub mod pll;

pub use error::*;

def_id!(ClkId, u64);

impl From<u32> for ClkId {
    fn from(value: u32) -> Self {
        Self(value as _)
    }
}

pub trait ResetOp {
    fn reset_assert(&mut self, id: RstId);

    fn reset_deassert(&mut self, id: RstId);

    fn reset_is_asserted(&self, id: RstId) -> Option<bool>;
}

pub trait ClockOp {
    /// 使能时钟
    ///
    /// 清除时钟门控 bit，使时钟输出到外设
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 成功返回 Ok(())，失败返回 Err
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// cru.clk_enable(CLK_I2C1)?;
    /// ```
    fn clk_enable(&mut self, id: ClkId) -> ClockResult<()>;

    /// 禁止时钟
    ///
    /// 设置时钟门控 bit，停止时钟输出
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 成功返回 Ok(())，失败返回 Err
    ///
    /// # 示例
    ///
    /// ```rust,ignore
    /// cru.clk_disable(CLK_I2C1)?;
    /// ```
    fn clk_disable(&mut self, id: ClkId) -> ClockResult<()>;

    /// 检查时钟是否已使能
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 返回 true 表示时钟已使能，false 表示已禁止，None 表示不支持
    fn clk_is_enabled(&self, id: ClkId) -> ClockResult<bool>;

    /// 获取时钟频率
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    ///
    /// # 返回
    ///
    /// 返回时钟频率 (Hz)，如果不支持该时钟则返回错误
    fn clk_get_rate(&self, id: crate::clock::ClkId) -> ClockResult<u64>;

    /// 设置时钟频率
    ///
    /// # 参数
    ///
    /// * `id` - 时钟 ID
    /// * `rate_hz` - 目标频率 (Hz)
    ///
    /// # 返回
    ///
    /// 返回实际设置的频率 (Hz)，如果不支持该时钟则返回错误
    fn clk_set_rate(&mut self, id: crate::clock::ClkId, rate_hz: u64) -> ClockResult<u64>;
}

pub enum Cru {
    Rk3568(crate::variants::rk3568::cru::Cru),
    Rk3588(crate::variants::rk3588::cru::Cru),
}

impl Cru {
    /// `base`: reg property
    /// `sys_grf`: "rockchip,grf"
    pub fn new(ty: SocType, base: Mmio, sys_grf: Mmio) -> Self {
        match ty {
            SocType::Rk3568 => Cru::Rk3568(crate::variants::rk3568::cru::Cru::new(base, sys_grf)),
            SocType::Rk3588 => Cru::Rk3588(crate::variants::rk3588::cru::Cru::new(base, sys_grf)),
        }
    }
}

impl ResetOp for Cru {
    fn reset_assert(&mut self, id: RstId) {
        match self {
            Self::Rk3568(cru) => cru.reset_assert(id),
            Self::Rk3588(cru) => cru.reset_assert(id),
        }
    }

    fn reset_deassert(&mut self, id: RstId) {
        match self {
            Self::Rk3568(cru) => cru.reset_deassert(id),
            Self::Rk3588(cru) => cru.reset_deassert(id),
        }
    }

    fn reset_is_asserted(&self, id: RstId) -> Option<bool> {
        match self {
            Self::Rk3568(cru) => cru.reset_is_asserted(id),
            Self::Rk3588(cru) => cru.reset_is_asserted(id),
        }
    }
}

impl ClockOp for Cru {
    fn clk_enable(&mut self, id: ClkId) -> ClockResult<()> {
        match self {
            Self::Rk3568(cru) => cru.clk_enable(id),
            Self::Rk3588(cru) => cru.clk_enable(id),
        }
    }

    fn clk_disable(&mut self, id: ClkId) -> ClockResult<()> {
        match self {
            Self::Rk3568(cru) => cru.clk_disable(id),
            Self::Rk3588(cru) => cru.clk_disable(id),
        }
    }

    fn clk_is_enabled(&self, id: ClkId) -> ClockResult<bool> {
        match self {
            Self::Rk3568(cru) => cru.clk_is_enabled(id),
            Self::Rk3588(cru) => cru.clk_is_enabled(id),
        }
    }

    fn clk_get_rate(&self, id: ClkId) -> ClockResult<u64> {
        match self {
            Self::Rk3568(cru) => cru.clk_get_rate(id),
            Self::Rk3588(cru) => cru.clk_get_rate(id),
        }
    }

    fn clk_set_rate(&mut self, id: ClkId, rate_hz: u64) -> ClockResult<u64> {
        match self {
            Self::Rk3568(cru) => cru.clk_set_rate(id, rate_hz),
            Self::Rk3588(cru) => cru.clk_set_rate(id, rate_hz),
        }
    }
}
