#[cfg(feature = "rockchip-dwmmc")]
pub use ax_drivers::soc::scmi;
pub use ax_drivers::soc::{
    RockchipPinCtrl, rk3588_enable_clock, rk3588_enable_power_domain, rk3588_reset_assert,
    rk3588_reset_deassert, rk3588_set_clock_rate,
};
