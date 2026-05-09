//! Pinctrl 通用类型定义
//!
//! 提供跨芯片的引脚控制抽象，包括引脚标识、配置类型和错误处理。

use core::fmt;

pub mod id;
mod pinconf;

pub use id::PinId;
pub use pinconf::{Iomux, PinConfig, Pull};

use crate::{Mmio, SocType};
pub(crate) mod gpio;

/// GPIO 方向配置（用于设置方向）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpioDirection {
    Input,
    Output(bool), // 携带初始输出值
}

/// Pinctrl 错误类型
#[derive(Debug)]
pub enum PinctrlError {
    /// 无效的引脚 ID
    InvalidPinId(PinId),

    /// 引脚不支持该功能
    InvalidFunction,

    /// 无效的引脚配置
    InvalidConfig,

    Unsupported,
}

impl fmt::Display for PinctrlError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidPinId(id) => write!(f, "无效的引脚 ID: {:?}", id),
            Self::InvalidFunction => write!(f, "引脚不支持该功能"),
            Self::InvalidConfig => write!(f, "无效的引脚配置"),
            Self::Unsupported => write!(f, "不支持的操作"),
        }
    }
}

/// Pinctrl 操作 Result 类型
pub type PinctrlResult<T> = core::result::Result<T, PinctrlError>;

#[enum_dispatch::enum_dispatch]
pub trait PinCtrlOp {
    fn set_config(&mut self, config: PinConfig) -> PinctrlResult<()>;

    fn get_config(&self, pin: PinId) -> PinctrlResult<PinConfig>;

    fn gpio_direction(&self, pin: PinId) -> PinctrlResult<GpioDirection>;

    fn set_gpio_direction(&self, pin: PinId, direction: GpioDirection) -> PinctrlResult<()>;

    fn read_gpio(&self, pin: PinId) -> PinctrlResult<bool>;

    /// 写入 GPIO 引脚值
    ///
    /// 引脚必须已配置为 GPIO 输出功能。
    ///
    /// # 参数
    ///
    /// * `pin` - 引脚 ID
    /// * `value` - 输出值（true = 高电平，false = 低电平）
    fn write_gpio(&self, pin: PinId, value: bool) -> PinctrlResult<()>;
}

#[enum_dispatch::enum_dispatch(PinCtrlOp)]
pub enum PinCtrl {
    Rk3588(crate::variants::rk3588::PinCtrl),
}

impl PinCtrl {
    pub fn new(ty: SocType, ioc: Mmio, gpio: &[Mmio]) -> Self {
        match ty {
            SocType::Rk3588 => PinCtrl::Rk3588(crate::variants::rk3588::PinCtrl::new(ioc, gpio)),
        }
    }
}
