#![no_std]

#[macro_use]
extern crate log;
#[macro_use]
extern crate axplat;
#[macro_use]
extern crate memory_addr;

mod config {
    axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
    assert_str_eq!(
        PACKAGE,
        env!("CARGO_PKG_NAME"),
        "`PACKAGE` field in the configuration does not match the Package name. Please check your configuration file."
    );
}

mod boot;
mod init;
mod mem;
mod power;

axplat_aarch64_peripherals::console_if_impl!(ConsoleIfImpl);
axplat_aarch64_peripherals::time_if_impl!(TimeIfImpl);

#[cfg(feature = "irq")]
axplat_aarch64_peripherals::irq_if_impl!(IrqIfImpl);
