#![no_std]

#[macro_use]
extern crate log;
#[macro_use]
extern crate axplat;
#[macro_use]
extern crate memory_addr;

mod config {
    axconfig_gen_macros::include_configs!("axconfig.toml");
}

mod boot;
mod init;
mod mem;
mod power;

axplat_aarch64_common::console_if_impl!(ConsoleIfImpl);
axplat_aarch64_common::time_if_impl!(TimeIfImpl);

#[cfg(feature = "irq")]
axplat_aarch64_common::irq_if_impl!(IrqIfImpl);
