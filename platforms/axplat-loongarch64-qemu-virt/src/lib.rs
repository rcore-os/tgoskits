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
mod console;
mod init;
#[cfg(feature = "irq")]
mod irq;
mod mem;
#[cfg(feature = "smp")]
mod mp;
mod power;
mod time;
