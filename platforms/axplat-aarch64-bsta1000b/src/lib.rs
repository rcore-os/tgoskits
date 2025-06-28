#![no_std]

#[macro_use]
extern crate log;
#[macro_use]
extern crate axplat;

pub mod config {
    //! Platform configuration module.
    //!
    //! If the `AX_CONFIG_PATH` environment variable is set, it will load the configuration from the specified path.
    //! Otherwise, it will fall back to the `axconfig.toml` file in the current directory and generate the default configuration.
    //!
    //! If the `PACKAGE` field in the configuration does not match the package name, it will panic with an error message.
    axconfig_macros::include_configs!(path_env = "AX_CONFIG_PATH", fallback = "axconfig.toml");
    assert_str_eq!(
        PACKAGE,
        env!("CARGO_PKG_NAME"),
        "`PACKAGE` field in the configuration does not match the Package name. Please check your configuration file."
    );
}

mod boot;
mod dw_apb_uart;
mod init;
mod mem;
mod misc;
#[cfg(feature = "smp")]
mod mp;
mod power;

axplat_aarch64_peripherals::time_if_impl!(TimeIfImpl);

#[cfg(feature = "irq")]
axplat_aarch64_peripherals::irq_if_impl!(IrqIfImpl);
