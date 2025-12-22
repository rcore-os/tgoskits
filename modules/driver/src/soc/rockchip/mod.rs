#[cfg(feature = "rk3588-clk")]
#[path = "clk/rk3588-clk.rs"]
mod clk;

#[cfg(feature = "rk3568-clk")]
#[path = "clk/rk3568-clk.rs"]
mod clk;
