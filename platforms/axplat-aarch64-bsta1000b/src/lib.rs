#![no_std]

#[macro_use]
extern crate log;
#[macro_use]
extern crate axplat;
#[macro_use]
extern crate memory_addr;

mod config {
    axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
}

mod boot;
mod dw_apb_uart;
mod init;
mod mem;
mod misc;
#[cfg(feature = "smp")]
mod mp;
mod power;

axplat_aarch64_common::time_if_impl!(TimeIfImpl);

#[cfg(feature = "irq")]
axplat_aarch64_common::irq_if_impl!(IrqIfImpl);
