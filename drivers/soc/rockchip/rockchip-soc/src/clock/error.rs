//! RK3588 CRU 时钟错误类型定义
//!
//! 使用 thiserror 提供结构化的错误处理，包含时钟 ID 上下文信息

#![allow(dead_code)]

use thiserror::Error;

use crate::clock::ClkId;

// =============================================================================
// 时钟操作错误类型
// =============================================================================

/// CRU 时钟操作错误
///
/// 包含详细的错误信息和相关的时钟 ID，便于调试和错误追踪
#[derive(Error, Debug)]
pub enum ClockError {
    /// 不支持的时钟 ID
    ///
    /// 当尝试操作一个不存在或未实现的时钟时返回
    #[error("unsupported: {clk_id}")]
    UnsupportedClock {
        /// 时钟 ID
        clk_id: ClkId,
    },

    /// 时钟频率设置失败
    ///
    /// 当请求的频率无法配置时返回
    #[error("failed to set clock {clk_id} to {rate_hz} Hz: unsupported rate")]
    InvalidRate {
        /// 目标时钟 ID
        clk_id: ClkId,
        /// 请求的频率 (Hz)
        rate_hz: u64,
    },

    /// 时钟频率读取失败
    ///
    /// 当无法读取时钟频率时返回
    #[error("failed to get clock {clk_id} rate: {reason}")]
    RateReadFailed {
        /// 目标时钟 ID
        clk_id: ClkId,
        /// 失败原因
        reason: &'static str,
    },

    /// 时钟使能失败
    ///
    /// 当无法使能时钟时返回
    #[error("failed to enable clock {clk_id}: {reason}")]
    EnableFailed {
        /// 目标时钟 ID
        clk_id: ClkId,
        /// 失败原因
        reason: &'static str,
    },

    /// 时钟禁用失败
    ///
    /// 当无法禁用时钟时返回
    #[error("failed to disable clock {clk_id}: {reason}")]
    DisableFailed {
        /// 目标时钟 ID
        clk_id: ClkId,
        /// 失败原因
        reason: &'static str,
    },

    /// PLL 配置错误
    ///
    /// 当 PLL 配置无效或无法设置时返回
    #[error("PLL configuration error for {clk_id}: {reason}")]
    PllConfigError {
        /// PLL 时钟 ID
        clk_id: ClkId,
        /// 失败原因
        reason: &'static str,
    },

    /// 时钟分频器配置错误
    ///
    /// 当分频器参数无效时返回
    #[error("invalid divider for clock {clk_id}: divisor must be > 0, got {divisor}")]
    InvalidDivider {
        /// 目标时钟 ID
        clk_id: ClkId,
        /// 无效的分频系数
        divisor: u32,
    },

    /// 时钟源选择错误
    ///
    /// 当选择的时钟源不可用时返回
    #[error("invalid clock source for {clk_id}: source {src} is not available")]
    InvalidClockSource {
        /// 目标时钟 ID
        clk_id: ClkId,
        /// 无效的时钟源索引
        src: u32,
    },
}

// =============================================================================
// 辅助构造函数
// =============================================================================

impl ClockError {
    /// 创建不支持时钟错误
    #[must_use]
    pub const fn unsupported(clk_id: ClkId) -> Self {
        Self::UnsupportedClock { clk_id }
    }

    /// 创建无效频率错误
    #[must_use]
    pub const fn invalid_rate(clk_id: ClkId, rate_hz: u64) -> Self {
        Self::InvalidRate { clk_id, rate_hz }
    }

    /// 创建频率读取失败错误
    #[must_use]
    pub const fn rate_read_failed(clk_id: ClkId, reason: &'static str) -> Self {
        Self::RateReadFailed { clk_id, reason }
    }

    /// 创建时钟使能失败错误
    #[must_use]
    pub const fn enable_failed(clk_id: ClkId, reason: &'static str) -> Self {
        Self::EnableFailed { clk_id, reason }
    }

    /// 创建时钟禁用失败错误
    #[must_use]
    pub const fn disable_failed(clk_id: ClkId, reason: &'static str) -> Self {
        Self::DisableFailed { clk_id, reason }
    }

    /// 创建 PLL 配置错误
    #[must_use]
    pub const fn pll_config_error(clk_id: ClkId, reason: &'static str) -> Self {
        Self::PllConfigError { clk_id, reason }
    }

    /// 创建无效分频器错误
    #[must_use]
    pub const fn invalid_divider(clk_id: ClkId, divisor: u32) -> Self {
        Self::InvalidDivider { clk_id, divisor }
    }

    /// 创建无效时钟源错误
    #[must_use]
    pub const fn invalid_clock_source(clk_id: ClkId, src: u32) -> Self {
        Self::InvalidClockSource { clk_id, src }
    }
}

// =============================================================================
// 时钟操作 Result 类型别名
// =============================================================================

/// 时钟操作 Result 类型
///
/// 用于所有时钟操作的返回值
pub type ClockResult<T> = core::result::Result<T, ClockError>;

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        use crate::rk3588::cru::clock::CLK_I2C0;

        let err = ClockError::unsupported(CLK_I2C0);
        assert_eq!(format!("{}", err), format!("unsupported: {}", CLK_I2C0));

        let err = ClockError::invalid_rate(CLK_I2C0, 100_000_000);
        assert_eq!(
            format!("{}", err),
            format!(
                "failed to set clock {} to 100000000 Hz: unsupported rate",
                CLK_I2C0
            )
        );

        let err = ClockError::rate_read_failed(CLK_I2C0, "register read timeout");
        assert_eq!(
            format!("{}", err),
            format!(
                "failed to get clock {} rate: register read timeout",
                CLK_I2C0
            )
        );
    }

    #[test]
    fn test_error_constructors() {
        use crate::rk3588::cru::clock::CLK_SPI0;

        let err = ClockError::unsupported(CLK_SPI0);
        match err {
            ClockError::UnsupportedClock { clk_id } => {
                assert_eq!(clk_id, CLK_SPI0);
            }
            _ => panic!("Unexpected error type"),
        }

        let err = ClockError::invalid_divider(CLK_SPI0, 0);
        match err {
            ClockError::InvalidDivider { clk_id, divisor } => {
                assert_eq!(clk_id, CLK_SPI0);
                assert_eq!(divisor, 0);
            }
            _ => panic!("Unexpected error type"),
        }
    }
}
