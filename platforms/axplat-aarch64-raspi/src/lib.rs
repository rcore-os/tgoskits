#![no_std]

#[macro_use]
extern crate axplat;

#[macro_use]
extern crate memory_addr;

mod boot;
mod init;
mod mem;
mod power;

#[cfg(feature = "smp")]
mod mp;

mod config {
    axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
}

axplat_aarch64_common::console_if_impl!(ConsoleIfImpl);
axplat_aarch64_common::time_if_impl!(TimeIfImpl);

#[cfg(feature = "irq")]
axplat_aarch64_common::irq_if_impl!(IrqIfImpl);
